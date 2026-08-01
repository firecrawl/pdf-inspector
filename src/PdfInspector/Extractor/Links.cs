// Ported from reference/src/extractor/links.rs
using System.Text;
using PdfInspector.Pdf;
using PdfInspector.Types;

namespace PdfInspector.Extractor;

/// <summary>Hyperlink and AcroForm field extraction from page annotations.</summary>
internal static class Links
{
    /// <summary>Emits one item per link annotation, positioned at the annotation's rectangle.</summary>
    public static List<TextItem> ExtractPageLinks(PdfDocument doc, PdfDictionary page, uint pageNum)
    {
        var links = new List<TextItem>();

        var annots = doc.GetDeref(page, "Annots")?.AsArray();
        if (annots is null)
        {
            return links;
        }

        foreach (var annotRef in annots)
        {
            var annot = doc.Resolve(annotRef).AsDictionary();
            if (annot is null)
            {
                continue;
            }

            // A missing subtype is tolerated, matching the reference: only an
            // explicit non-Link subtype disqualifies the annotation.
            if (annot.Get("Subtype")?.AsName() is { } subtype && subtype != "Link")
            {
                continue;
            }

            if (ReadRect(doc, annot) is not { } rect)
            {
                continue;
            }

            if (ExtractLinkUri(doc, annot) is not { } url)
            {
                continue;
            }

            links.Add(new TextItem
            {
                Text = url,
                X = rect.X,
                Y = rect.Y,
                Width = rect.Width,
                Height = rect.Height,
                Page = pageNum,
                Kind = ItemKind.Link,
                LinkUrl = url,
            });
        }

        return links;
    }

    private static (float X, float Y, float Width, float Height)? ReadRect(PdfDocument doc, PdfDictionary annot)
    {
        var array = doc.GetDeref(annot, "Rect")?.AsArray();
        if (array is null || array.Count < 4)
        {
            return null;
        }

        var x1 = Geometry.GetNumber(array[0]) ?? 0.0f;
        var y1 = Geometry.GetNumber(array[1]) ?? 0.0f;
        var x2 = Geometry.GetNumber(array[2]) ?? 0.0f;
        var y2 = Geometry.GetNumber(array[3]) ?? 0.0f;

        return (x1, y1, x2 - x1, y2 - y1);
    }

    /// <summary>
    /// Reads the target of a link annotation's URI action. Named destinations
    /// are not resolved: they address places inside the document rather than
    /// external targets.
    /// </summary>
    public static string? ExtractLinkUri(PdfDocument doc, PdfDictionary annot)
    {
        var action = doc.GetDeref(annot, "A")?.AsDictionary();
        if (action is null)
        {
            return null;
        }

        return action.Get("URI")?.AsStringBytes() is { } uri
            ? Encoding.UTF8.GetString(uri)
            : null;
    }

    /// <summary>
    /// Extracts AcroForm field values as items positioned at each field's
    /// rectangle, so they flow into the markdown pipeline alongside page text.
    /// </summary>
    public static List<TextItem> ExtractFormFields(PdfDocument doc, IReadOnlyDictionary<PdfObjectId, uint> pageMap)
    {
        var items = new List<TextItem>();

        var root = doc.Catalog;
        if (root is null)
        {
            return items;
        }

        var acroForm = doc.GetDict(root, "AcroForm");
        if (acroForm is null)
        {
            return items;
        }

        var fields = doc.GetArray(acroForm, "Fields");
        if (fields is null)
        {
            return items;
        }

        var visited = new HashSet<PdfObjectId>();
        foreach (var field in fields)
        {
            if (field.AsReference() is { } fieldId)
            {
                WalkFormFields(doc, fieldId, null, string.Empty, pageMap, items, visited);
            }
        }

        return items;
    }

    /// <summary>Walks the field tree, emitting one item per leaf field that carries a value.</summary>
    private static void WalkFormFields(
        PdfDocument doc,
        PdfObjectId fieldId,
        string? parentFieldType,
        string parentName,
        IReadOnlyDictionary<PdfObjectId, uint> pageMap,
        List<TextItem> items,
        HashSet<PdfObjectId> visited)
    {
        // A malformed file can make the field tree cyclic.
        if (!visited.Add(fieldId))
        {
            return;
        }

        var field = doc.GetObject(fieldId)?.AsDictionary();
        if (field is null)
        {
            return;
        }

        var localName = field.Get("T")?.AsStringBytes() is { } t ? Encoding.UTF8.GetString(t) : string.Empty;

        var fullName = parentName.Length == 0
            ? localName
            : localName.Length == 0 ? parentName : $"{parentName}.{localName}";

        // The field type is inheritable from the parent.
        var fieldType = field.Get("FT")?.AsName() ?? parentFieldType;

        if (doc.GetArray(field, "Kids") is { } kids)
        {
            foreach (var kid in kids)
            {
                if (kid.AsReference() is { } kidId)
                {
                    WalkFormFields(doc, kidId, fieldType, fullName, pageMap, items, visited);
                }
            }

            return;
        }

        if (fieldType is null || fieldType == "Sig")
        {
            return;
        }

        var value = field.Get("V");
        if (value is null)
        {
            return;
        }

        var valueText = FormatFieldValue(fieldType, value);
        if (valueText is null)
        {
            return;
        }

        var (x, y, width, height) = ReadFieldRect(doc, field);

        var pageNum = field.Get("P")?.AsReference() is { } pageRef && pageMap.TryGetValue(pageRef, out var mapped)
            ? mapped
            : 1u;

        items.Add(new TextItem
        {
            Text = fullName.Length == 0 ? valueText : $"{fullName}: {valueText}",
            X = x,
            Y = y,
            Width = width,
            Height = height,
            Page = pageNum,
            Kind = ItemKind.FormField,
        });
    }

    private static string? FormatFieldValue(string fieldType, PdfObject value)
    {
        switch (fieldType)
        {
            case "Tx":
            case "Ch":
            {
                if (value is PdfString str)
                {
                    var text = Encoding.UTF8.GetString(str.Bytes);
                    return text.Length == 0 ? null : text;
                }

                if (value is PdfArray array)
                {
                    var parts = array
                        .OfType<PdfString>()
                        .Select(s => Encoding.UTF8.GetString(s.Bytes))
                        .ToList();

                    return parts.Count == 0 ? null : string.Join(", ", parts);
                }

                return null;
            }

            case "Btn":
            {
                // Checkbox and radio values are names; "Off" means unset.
                if (value.AsName() is not { } name || name == "Off")
                {
                    return null;
                }

                return name is "Yes" or "1" ? "Yes" : name;
            }

            default:
                return null;
        }
    }

    private static (float X, float Y, float Width, float Height) ReadFieldRect(PdfDocument doc, PdfDictionary field)
    {
        var array = doc.GetDeref(field, "Rect")?.AsArray();
        if (array is null || array.Count < 4)
        {
            return (0.0f, 0.0f, 0.0f, 0.0f);
        }

        var x1 = Geometry.GetNumber(array[0]) ?? 0.0f;
        var y1 = Geometry.GetNumber(array[1]) ?? 0.0f;
        var x2 = Geometry.GetNumber(array[2]) ?? 0.0f;
        var y2 = Geometry.GetNumber(array[3]) ?? 0.0f;

        return (x1, MathF.Min(y1, y2), MathF.Abs(x2 - x1), MathF.Abs(y2 - y1));
    }
}
