// Ported from reference/src/structure_tree.rs
namespace PdfInspector.Structure;

/// <summary>
/// A standard PDF structure element type (ISO 32000-1, tables 333–340).
/// Non-standard tags are carried through as <see cref="Kind.Other"/> with the
/// original name preserved.
/// </summary>
public sealed record StructRole
{
    public enum Kind
    {
        Document, Part, Art, Sect, Div, BlockQuote, Caption, Toc, Toci, Index,
        NonStruct, Private,

        // Heading and paragraph
        H, H1, H2, H3, H4, H5, H6, P,

        // List
        L, Li, Lbl, LBody,

        // Table
        Table, Tr, Th, Td, THead, TBody, TFoot,

        // Inline
        Span, Quote, Note, Reference, BibEntry, Code, Link, Annot,

        // Illustration
        Figure, Formula, Form,

        // Ruby and Warichu (CJK)
        Ruby, Rb, Rt, Rp, Warichu, Wt, Wp,

        // Fallback
        Other,
    }

    private StructRole(Kind role, string? name = null)
    {
        Role = role;
        Name = name;
    }

    public Kind Role { get; }

    /// <summary>The original tag name; set only when <see cref="Role"/> is <see cref="Kind.Other"/>.</summary>
    public string? Name { get; }

    private static readonly Dictionary<Kind, StructRole> Cache =
        Enum.GetValues<Kind>().Where(k => k != Kind.Other).ToDictionary(k => k, k => new StructRole(k));

    public static StructRole Of(Kind kind) => Cache[kind];

    public static StructRole Other(string name) => new(Kind.Other, name);

    /// <summary>
    /// Content roles whose text must never be promoted to a heading by the
    /// visual heuristic. These carry an explicit non-heading meaning in the
    /// structure tree (lists, quotes, notes, references, captions, formulas,
    /// forms, table-of-contents entries), yet their text is often short and
    /// visually isolated — exactly what the heuristic keys on. Heading roles
    /// (H, H1–H6) and generic container roles (P, Div, Sect, Span, …) are
    /// excluded so the heuristic can still fire there.
    ///
    /// Figure is deliberately absent: cover and banner pages routinely tag the
    /// document title inside a Figure alongside a seal or logo, and that title
    /// is a real heading. Formula and Form stay — a line explicitly tagged as
    /// an equation or form field is never a heading.
    ///
    /// Table roles are included so that when table reconstruction falls back
    /// and cells reach the line loop as plain text, a short isolated cell — a
    /// TH column header especially — is not promoted to a heading.
    /// </summary>
    public bool IsNonHeadingContent => Role is
        Kind.L or Kind.Li or Kind.Lbl or Kind.LBody
        or Kind.BlockQuote or Kind.Quote or Kind.Caption
        or Kind.Toc or Kind.Toci or Kind.Index
        or Kind.Note or Kind.Reference or Kind.BibEntry
        or Kind.Code or Kind.Formula or Kind.Form
        or Kind.Table or Kind.Tr or Kind.Th or Kind.Td
        or Kind.THead or Kind.TBody or Kind.TFoot;

    public static StructRole FromName(string name) => name switch
    {
        "Document" => Of(Kind.Document),
        "Part" => Of(Kind.Part),
        "Art" => Of(Kind.Art),
        "Sect" => Of(Kind.Sect),
        "Div" => Of(Kind.Div),
        "BlockQuote" => Of(Kind.BlockQuote),
        "Caption" => Of(Kind.Caption),
        "TOC" => Of(Kind.Toc),
        "TOCI" => Of(Kind.Toci),
        "Index" => Of(Kind.Index),
        "NonStruct" => Of(Kind.NonStruct),
        "Private" => Of(Kind.Private),
        "H" => Of(Kind.H),
        "H1" => Of(Kind.H1),
        "H2" => Of(Kind.H2),
        "H3" => Of(Kind.H3),
        "H4" => Of(Kind.H4),
        "H5" => Of(Kind.H5),
        "H6" => Of(Kind.H6),
        "P" => Of(Kind.P),
        "L" => Of(Kind.L),
        "LI" => Of(Kind.Li),
        "Lbl" => Of(Kind.Lbl),
        "LBody" => Of(Kind.LBody),
        "Table" => Of(Kind.Table),
        "TR" => Of(Kind.Tr),
        "TH" => Of(Kind.Th),
        "TD" => Of(Kind.Td),
        "THead" => Of(Kind.THead),
        "TBody" => Of(Kind.TBody),
        "TFoot" => Of(Kind.TFoot),
        "Span" => Of(Kind.Span),
        "Quote" => Of(Kind.Quote),
        "Note" => Of(Kind.Note),
        "Reference" => Of(Kind.Reference),
        "BibEntry" => Of(Kind.BibEntry),
        "Code" => Of(Kind.Code),
        "Link" => Of(Kind.Link),
        "Annot" => Of(Kind.Annot),
        "Figure" => Of(Kind.Figure),
        "Formula" => Of(Kind.Formula),
        "Form" => Of(Kind.Form),
        "Ruby" => Of(Kind.Ruby),
        "RB" => Of(Kind.Rb),
        "RT" => Of(Kind.Rt),
        "RP" => Of(Kind.Rp),
        "Warichu" => Of(Kind.Warichu),
        "WT" => Of(Kind.Wt),
        "WP" => Of(Kind.Wp),
        _ => Other(name),
    };

    /// <summary>Resolves a possibly-custom tag through the document's role map.</summary>
    public static StructRole FromNameWithRoleMap(string name, IReadOnlyDictionary<string, string> roleMap)
    {
        var current = name;

        // Bounded so a cyclic role map cannot loop forever.
        for (var i = 0; i < 8; i++)
        {
            var role = FromName(current);
            if (role.Role != Kind.Other)
            {
                return role;
            }

            if (!roleMap.TryGetValue(current, out var mapped))
            {
                return role;
            }

            current = mapped;
        }

        return Other(name);
    }

    public override string ToString() => Role == Kind.Other ? Name ?? "Other" : Role.ToString();
}
