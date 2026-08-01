// Ported from reference/src/types.rs
namespace PdfInspector.Types;

/// <summary>Type of extracted item.</summary>
public enum ItemKind
{
    /// <summary>Regular text content.</summary>
    Text,

    /// <summary>Image placeholder.</summary>
    Image,

    /// <summary>Hyperlink; the target lives in <see cref="TextItem.LinkUrl"/>.</summary>
    Link,

    /// <summary>Form field (name: value).</summary>
    FormField,
}

/// <summary>A line segment from PDF path operators (<c>m</c>/<c>l</c>/<c>S</c>).</summary>
public sealed class PdfLine
{
    public float X1;
    public float Y1;
    public float X2;
    public float Y2;
    public uint Page;

    public PdfLine()
    {
    }

    public PdfLine(float x1, float y1, float x2, float y2, uint page)
    {
        X1 = x1;
        Y1 = y1;
        X2 = x2;
        Y2 = y2;
        Page = page;
    }

    public PdfLine Clone() => new(X1, Y1, X2, Y2, Page);
}

/// <summary>A rectangle from a PDF <c>re</c> operator (cell boundary, border, etc.).</summary>
public sealed class PdfRect
{
    public float X;
    public float Y;
    public float Width;
    public float Height;
    public uint Page;

    public PdfRect()
    {
    }

    public PdfRect(float x, float y, float width, float height, uint page)
    {
        X = x;
        Y = y;
        Width = width;
        Height = height;
        Page = page;
    }

    public PdfRect Clone() => new(X, Y, Width, Height, Page);
}

/// <summary>A text item with position information.</summary>
public sealed class TextItem
{
    /// <summary>The text content.</summary>
    public string Text = string.Empty;

    /// <summary>X position on page.</summary>
    public float X;

    /// <summary>Y position on page (PDF coordinates, origin at bottom-left).</summary>
    public float Y;

    /// <summary>Width of text.</summary>
    public float Width;

    /// <summary>Height (approximated from font size).</summary>
    public float Height;

    /// <summary>Font name.</summary>
    public string Font = string.Empty;

    /// <summary>Font size.</summary>
    public float FontSize;

    /// <summary>Page number (1-indexed).</summary>
    public uint Page;

    /// <summary>Whether the font is bold.</summary>
    public bool IsBold;

    /// <summary>Whether the font is italic.</summary>
    public bool IsItalic;

    /// <summary>
    /// Whether the text is underlined. PDFs have no underline font flag, so this
    /// is detected geometrically after extraction; see <c>Extractor/Underline</c>.
    /// </summary>
    public bool IsUnderline;

    /// <summary>
    /// Whether the text is struck out. Same geometric detection as underline,
    /// different vertical window.
    /// </summary>
    public bool IsStrikeout;

    /// <summary>Type of item (text, image, link).</summary>
    public ItemKind Kind = ItemKind.Text;

    /// <summary>Target URL when <see cref="Kind"/> is <see cref="ItemKind.Link"/>.</summary>
    public string? LinkUrl;

    /// <summary>
    /// Marked Content ID from the content stream's BDC/BMC operator, used to link
    /// this item to the structure tree of a tagged PDF.
    /// </summary>
    public long? Mcid;

    public TextItem Clone() => new()
    {
        Text = Text,
        X = X,
        Y = Y,
        Width = Width,
        Height = Height,
        Font = Font,
        FontSize = FontSize,
        Page = Page,
        IsBold = IsBold,
        IsItalic = IsItalic,
        IsUnderline = IsUnderline,
        IsStrikeout = IsStrikeout,
        Kind = Kind,
        LinkUrl = LinkUrl,
        Mcid = Mcid,
    };
}

/// <summary>
/// Layout complexity analysis result. Callers use this to decide whether the
/// extracted markdown is reliable or whether the PDF should be routed to OCR.
/// </summary>
public sealed class LayoutComplexity
{
    /// <summary>True if any page has tables or multi-column text.</summary>
    public bool IsComplex;

    /// <summary>1-indexed pages where table borders were detected (rect count &gt; 6).</summary>
    public List<uint> PagesWithTables = [];

    /// <summary>1-indexed pages where 2+ text columns were detected.</summary>
    public List<uint> PagesWithColumns = [];
}
