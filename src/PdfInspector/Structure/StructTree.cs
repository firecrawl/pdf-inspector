// Ported from reference/src/structure_tree.rs
using PdfInspector.Pdf;
using PdfInspector.Text;

namespace PdfInspector.Structure;

/// <summary>A leaf reference linking a structure element to content-stream content.</summary>
public sealed class MarkedContentRef
{
    /// <summary>The marked-content id used by the content stream's BDC/BMC operators.</summary>
    public required long Mcid { get; init; }

    /// <summary>Page this content belongs to, from the element's <c>/Pg</c> key.</summary>
    public PdfObjectId? PageId { get; init; }
}

/// <summary>A node in the PDF structure tree.</summary>
public sealed class StructElement
{
    public required StructRole Role { get; init; }

    /// <summary>Alternative text for figures and illustrations.</summary>
    public string? AltText { get; init; }

    /// <summary>Actual-text override, used for ligatures among other things.</summary>
    public string? ActualText { get; init; }

    /// <summary>Language override, such as "en-US".</summary>
    public string? Lang { get; init; }

    /// <summary>Marked-content references owned directly by this element.</summary>
    public List<MarkedContentRef> ContentRefs { get; init; } = [];

    public List<StructElement> Children { get; init; } = [];
}

/// <summary>A flattened view of a structure element, for linear traversal.</summary>
public sealed class FlatStructElement
{
    public required StructRole Role { get; init; }

    /// <summary>Nesting depth, zero for a top-level element.</summary>
    public required int Depth { get; init; }

    public string? AltText { get; init; }

    public List<MarkedContentRef> ContentRefs { get; init; } = [];

    /// <summary>How many children the element had in the original tree.</summary>
    public required int ChildCount { get; init; }
}

/// <summary>A table cell recovered from the structure tree.</summary>
public sealed class StructTableCell
{
    public required bool IsHeader { get; init; }

    /// <summary>Marked-content ids paired with their resolved 1-indexed page numbers.</summary>
    public List<(long Mcid, uint Page)> Mcids { get; init; } = [];
}

public sealed class StructTableRow
{
    public List<StructTableCell> Cells { get; init; } = [];
}

public sealed class StructTable
{
    public List<StructTableRow> Rows { get; init; } = [];
}

/// <summary>
/// A parsed tagged-PDF structure tree, built from <c>/StructTreeRoot</c> in the
/// document catalog. Leaves map back to content-stream marked content by id,
/// which lets the markdown pipeline attach semantic roles to extracted text.
/// </summary>
public sealed class StructTree
{
    private const string Module = "structure";

    /// <summary>Bounds recursion on malformed or cyclic trees.</summary>
    private const int MaxDepth = 64;

    public List<StructElement> Children { get; init; } = [];

    /// <summary>Parses the structure tree, or returns null when the PDF is not tagged.</summary>
    public static StructTree? FromDocument(PdfDocument doc)
    {
        var catalog = doc.Catalog;
        if (catalog is null)
        {
            return null;
        }

        var structRoot = doc.GetDict(catalog, "StructTreeRoot");
        if (structRoot is null)
        {
            return null;
        }

        var roleMap = ParseRoleMap(doc, structRoot);
        Log.Debug(Module, () => $"structure tree: {roleMap.Count} role map entries");

        var children = ParseKids(doc, structRoot, roleMap, null, 0);
        Log.Debug(Module, () => $"structure tree: {children.Count} top-level elements");

        return children.Count == 0 ? null : new StructTree { Children = children };
    }

    /// <summary>
    /// Builds a per-page lookup from marked-content id to role, keyed by
    /// 1-indexed page number.
    /// </summary>
    public Dictionary<uint, Dictionary<long, StructRole>> McidToRoles(IReadOnlyList<PdfObjectId> pageIds)
    {
        var objToPage = new Dictionary<PdfObjectId, uint>();
        for (var i = 0; i < pageIds.Count; i++)
        {
            objToPage[pageIds[i]] = (uint)(i + 1);
        }

        var result = new Dictionary<uint, Dictionary<long, StructRole>>();
        CollectMcidRoles(Children, objToPage, result);
        return result;
    }

    private static void CollectMcidRoles(
        List<StructElement> elements,
        Dictionary<PdfObjectId, uint> objToPage,
        Dictionary<uint, Dictionary<long, StructRole>> result)
    {
        foreach (var element in elements)
        {
            foreach (var mcref in element.ContentRefs)
            {
                if (mcref.PageId is not { } pageId || !objToPage.TryGetValue(pageId, out var pageNum))
                {
                    continue;
                }

                if (!result.TryGetValue(pageNum, out var byMcid))
                {
                    byMcid = [];
                    result[pageNum] = byMcid;
                }

                byMcid[mcref.Mcid] = element.Role;
            }

            CollectMcidRoles(element.Children, objToPage, result);
        }
    }

    /// <summary>Total number of marked-content references in the tree.</summary>
    public int McidCount => CountRefs(Children);

    private static int CountRefs(List<StructElement> elements)
    {
        var total = 0;
        foreach (var element in elements)
        {
            total += element.ContentRefs.Count + CountRefs(element.Children);
        }

        return total;
    }

    /// <summary>Flattens the tree into document order, recording each element's depth.</summary>
    public List<FlatStructElement> Flatten()
    {
        var output = new List<FlatStructElement>();
        FlattenRecursive(Children, output, 0);
        return output;
    }

    private static void FlattenRecursive(List<StructElement> elements, List<FlatStructElement> output, int depth)
    {
        foreach (var element in elements)
        {
            output.Add(new FlatStructElement
            {
                Role = element.Role,
                Depth = depth,
                AltText = element.AltText,
                ContentRefs = [.. element.ContentRefs],
                ChildCount = element.Children.Count,
            });

            FlattenRecursive(element.Children, output, depth + 1);
        }
    }

    /// <summary>
    /// Extracts table structures from the tree: <c>/Table</c> elements with
    /// <c>/TR</c> rows of <c>/TD</c> or <c>/TH</c> cells, collecting the
    /// marked-content ids at each cell so tables can be built without geometry.
    /// </summary>
    public List<StructTable> ExtractTables(IReadOnlyList<PdfObjectId> pageIds)
    {
        var objToPage = new Dictionary<PdfObjectId, uint>();
        for (var i = 0; i < pageIds.Count; i++)
        {
            objToPage[pageIds[i]] = (uint)(i + 1);
        }

        var tables = new List<StructTable>();
        CollectTables(Children, objToPage, tables);
        return tables;
    }

    private static void CollectTables(
        List<StructElement> elements,
        Dictionary<PdfObjectId, uint> objToPage,
        List<StructTable> tables)
    {
        foreach (var element in elements)
        {
            if (element.Role.Role == StructRole.Kind.Table)
            {
                var rows = new List<StructTableRow>();
                CollectRows(element.Children, objToPage, rows);
                if (rows.Count >= 2 && rows.Any(r => r.Cells.Count > 0))
                {
                    tables.Add(new StructTable { Rows = rows });
                }
            }
            else
            {
                CollectTables(element.Children, objToPage, tables);
            }
        }
    }

    /// <summary>Collects rows, descending transparently through THead/TBody/TFoot grouping.</summary>
    private static void CollectRows(
        List<StructElement> elements,
        Dictionary<PdfObjectId, uint> objToPage,
        List<StructTableRow> rows)
    {
        foreach (var element in elements)
        {
            switch (element.Role.Role)
            {
                case StructRole.Kind.Tr:
                {
                    var cells = new List<StructTableCell>();
                    foreach (var child in element.Children)
                    {
                        if (child.Role.Role is not (StructRole.Kind.Td or StructRole.Kind.Th))
                        {
                            continue;
                        }

                        var mcids = new List<(long, uint)>();
                        CollectMcidsRecursive(child, objToPage, mcids);
                        cells.Add(new StructTableCell
                        {
                            IsHeader = child.Role.Role == StructRole.Kind.Th,
                            Mcids = mcids,
                        });
                    }

                    rows.Add(new StructTableRow { Cells = cells });
                    break;
                }

                case StructRole.Kind.THead:
                case StructRole.Kind.TBody:
                case StructRole.Kind.TFoot:
                    CollectRows(element.Children, objToPage, rows);
                    break;
            }
        }
    }

    private static void CollectMcidsRecursive(
        StructElement element,
        Dictionary<PdfObjectId, uint> objToPage,
        List<(long, uint)> mcids)
    {
        foreach (var mcref in element.ContentRefs)
        {
            if (mcref.PageId is { } pageId && objToPage.TryGetValue(pageId, out var pageNum))
            {
                mcids.Add((mcref.Mcid, pageNum));
            }
        }

        foreach (var child in element.Children)
        {
            CollectMcidsRecursive(child, objToPage, mcids);
        }
    }

    // ── Parsing ──────────────────────────────────────────────────────────

    /// <summary>Reads the <c>/RoleMap</c> dictionary, mapping custom tags to standard ones.</summary>
    private static Dictionary<string, string> ParseRoleMap(PdfDocument doc, PdfDictionary structRoot)
    {
        var map = new Dictionary<string, string>(StringComparer.Ordinal);
        var roleMap = doc.GetDict(structRoot, "RoleMap");
        if (roleMap is null)
        {
            return map;
        }

        foreach (var (key, value) in roleMap)
        {
            if (value.AsName() is { } name)
            {
                map[key] = name;
            }
        }

        return map;
    }

    private static List<StructElement> ParseKids(
        PdfDocument doc,
        PdfDictionary dict,
        Dictionary<string, string> roleMap,
        PdfObjectId? inheritedPage,
        int depth)
    {
        if (depth >= MaxDepth)
        {
            return [];
        }

        var kids = dict.Get("K");
        if (kids is null)
        {
            return [];
        }

        // /Pg on this element is inherited by its children.
        var pageId = GetPageRef(doc, dict) ?? inheritedPage;
        var children = new List<StructElement>();

        if (kids is PdfArray array)
        {
            foreach (var item in array)
            {
                ParseKid(doc, doc.Resolve(item), roleMap, pageId, depth, children);
            }
        }
        else
        {
            ParseKid(doc, doc.Resolve(kids), roleMap, pageId, depth, children);
        }

        return children;
    }

    /// <summary>Parses a single child, which may be a structure element or a bare marked-content id.</summary>
    private static void ParseKid(
        PdfDocument doc,
        PdfObject obj,
        Dictionary<string, string> roleMap,
        PdfObjectId? inheritedPage,
        int depth,
        List<StructElement> output)
    {
        switch (obj)
        {
            case PdfInteger mcid:
                // A bare marked-content id at element level; wrap it as a Span leaf.
                output.Add(new StructElement
                {
                    Role = StructRole.Of(StructRole.Kind.Span),
                    ContentRefs = [new MarkedContentRef { Mcid = mcid.Value, PageId = inheritedPage }],
                });
                break;

            case PdfStream stream:
                // A few producers wrap structure elements in streams.
                ParseStructElementDict(doc, stream.Dictionary, roleMap, inheritedPage, depth, output);
                break;

            case PdfDictionary dict:
                ParseStructElementDict(doc, dict, roleMap, inheritedPage, depth, output);
                break;
        }
    }

    /// <summary>
    /// Parses a dictionary that is either a structure element or a
    /// marked-content reference.
    /// </summary>
    private static void ParseStructElementDict(
        PdfDocument doc,
        PdfDictionary dict,
        Dictionary<string, string> roleMap,
        PdfObjectId? inheritedPage,
        int depth,
        List<StructElement> output)
    {
        if (depth >= MaxDepth)
        {
            return;
        }

        if (IsMcrDict(dict))
        {
            if (dict.Get("MCID")?.AsInteger() is { } mcid)
            {
                output.Add(new StructElement
                {
                    Role = StructRole.Of(StructRole.Kind.Span),
                    ContentRefs =
                    [
                        new MarkedContentRef { Mcid = mcid, PageId = GetPageRef(doc, dict) ?? inheritedPage },
                    ],
                });
            }

            return;
        }

        // Object references carry no content of their own.
        if (IsObjrDict(dict))
        {
            return;
        }

        var roleName = doc.Resolve(dict.Get("S")).AsName();
        if (roleName is null)
        {
            return;
        }

        var role = StructRole.FromNameWithRoleMap(roleName, roleMap);
        var pageId = GetPageRef(doc, dict) ?? inheritedPage;

        var contentRefs = new List<MarkedContentRef>();
        var children = new List<StructElement>();

        if (dict.Get("K") is { } kids)
        {
            var resolved = doc.Resolve(kids);
            switch (resolved)
            {
                case PdfInteger mcid:
                    contentRefs.Add(new MarkedContentRef { Mcid = mcid.Value, PageId = pageId });
                    break;

                case PdfArray array:
                    foreach (var item in array)
                    {
                        ParseKidEntry(doc, doc.Resolve(item), roleMap, pageId, depth, contentRefs, children);
                    }

                    break;

                case PdfDictionary child:
                    if (IsMcrDict(child))
                    {
                        if (child.Get("MCID")?.AsInteger() is { } childMcid)
                        {
                            contentRefs.Add(new MarkedContentRef
                            {
                                Mcid = childMcid,
                                PageId = GetPageRef(doc, child) ?? pageId,
                            });
                        }
                    }
                    else
                    {
                        ParseStructElementDict(doc, child, roleMap, pageId, depth + 1, children);
                    }

                    break;
            }
        }

        output.Add(new StructElement
        {
            Role = role,
            AltText = GetTextString(dict, "Alt"),
            ActualText = GetTextString(dict, "ActualText"),
            Lang = GetTextString(dict, "Lang"),
            ContentRefs = contentRefs,
            Children = children,
        });
    }

    private static void ParseKidEntry(
        PdfDocument doc,
        PdfObject resolved,
        Dictionary<string, string> roleMap,
        PdfObjectId? pageId,
        int depth,
        List<MarkedContentRef> contentRefs,
        List<StructElement> children)
    {
        switch (resolved)
        {
            case PdfInteger mcid:
                contentRefs.Add(new MarkedContentRef { Mcid = mcid.Value, PageId = pageId });
                break;

            case PdfStream stream:
                ParseStructElementDict(doc, stream.Dictionary, roleMap, pageId, depth + 1, children);
                break;

            case PdfDictionary dict:
                if (IsMcrDict(dict))
                {
                    if (dict.Get("MCID")?.AsInteger() is { } childMcid)
                    {
                        contentRefs.Add(new MarkedContentRef
                        {
                            Mcid = childMcid,
                            PageId = GetPageRef(doc, dict) ?? pageId,
                        });
                    }
                }
                else if (!IsObjrDict(dict))
                {
                    ParseStructElementDict(doc, dict, roleMap, pageId, depth + 1, children);
                }

                break;
        }
    }

    private static bool IsMcrDict(PdfDictionary dict) => dict.Get("Type")?.AsName() == "MCR";

    private static bool IsObjrDict(PdfDictionary dict) => dict.Get("Type")?.AsName() == "OBJR";

    private static PdfObjectId? GetPageRef(PdfDocument doc, PdfDictionary dict)
    {
        var pg = dict.Get("Pg");
        if (pg is null)
        {
            return null;
        }

        return pg.AsReference() ?? doc.Resolve(pg).AsReference();
    }

    private static string? GetTextString(PdfDictionary dict, string key) =>
        dict.Get(key) is PdfString str ? TextUtils.DecodeTextString(str.Bytes) : null;
}
