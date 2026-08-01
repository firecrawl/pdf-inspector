// Ported from reference/src/types.rs
using System.Text;
using PdfInspector.Text;

namespace PdfInspector.Types;

/// <summary>A line of text: the items grouped onto one baseline.</summary>
public sealed class TextLine
{
    public List<TextItem> Items = [];

    public float Y;

    public uint Page;

    /// <summary>
    /// Adaptive join threshold from page-level letter-spacing detection.
    /// <see cref="TextUtils.DefaultJoinThreshold"/> for ordinary PDFs; higher for
    /// pages rendered character-by-character.
    /// </summary>
    public float AdaptiveThreshold = TextUtils.DefaultJoinThreshold;

    public string Text() => TextWithFormatting(false, false, false);

    /// <summary>Renders the line, optionally emitting bold, italic, and underline markers.</summary>
    public string TextWithFormatting(bool formatBold, bool formatItalic, bool formatUnderline)
    {
        if (!formatBold && !formatItalic && !formatUnderline)
        {
            return TextPlain();
        }

        var singleCharThreshold = AdaptiveThreshold;

        var result = new StringBuilder();
        var currentBold = false;
        var currentItalic = false;
        var currentUnderline = false;

        for (var i = 0; i < Items.Count; i++)
        {
            var item = Items[i];
            var text = item.Text;
            var textTrimmed = text.Trim();

            if (textTrimmed.Length == 0)
            {
                continue;
            }

            var needsSpace = i != 0 && result.Length != 0 &&
                NeedsSpaceBetween(Items[i - 1], item, result, singleCharThreshold);

            // Items such as " means any person" carry a leading space that marks a
            // word boundary. NeedsSpaceBetween returns false for those (a space
            // already exists), but the trimmed text below would drop it.
            var hasLeadingSpace = text.StartsWith(' ');

            // Underline is exclusive: `<u>` content stays free of `**`/`*` markers,
            // because consumers match the tag content literally and mixed
            // `<u>**x**</u>` nesting breaks that.
            var itemUnderline = formatUnderline && item.IsUnderline;
            var itemBold = formatBold && item.IsBold && !itemUnderline;
            var itemItalic = formatItalic && item.IsItalic && !itemUnderline;

            if (currentItalic && !itemItalic)
            {
                result.Append('*');
                currentItalic = false;
            }

            if (currentBold && !itemBold)
            {
                result.Append("**");
                currentBold = false;
            }

            if (currentUnderline && !itemUnderline)
            {
                result.Append("</u>");
                currentUnderline = false;
            }

            if (needsSpace || (hasLeadingSpace && result.Length != 0 && result[^1] != ' '))
            {
                result.Append(' ');
            }

            if (itemUnderline && !currentUnderline)
            {
                result.Append("<u>");
                currentUnderline = true;
            }

            if (itemBold && !currentBold)
            {
                result.Append("**");
                currentBold = true;
            }

            if (itemItalic && !currentItalic)
            {
                result.Append('*');
                currentItalic = true;
            }

            result.Append(textTrimmed);
        }

        if (currentItalic)
        {
            result.Append('*');
        }

        if (currentBold)
        {
            result.Append("**");
        }

        if (currentUnderline)
        {
            result.Append("</u>");
        }

        return result.ToString();
    }

    private string TextPlain()
    {
        var singleCharThreshold = AdaptiveThreshold;

        var result = new StringBuilder();
        for (var i = 0; i < Items.Count; i++)
        {
            var text = Items[i].Text;
            if (i == 0)
            {
                result.Append(text);
                continue;
            }

            if (NeedsSpaceBetween(Items[i - 1], Items[i], result, singleCharThreshold))
            {
                result.Append(' ');
            }

            result.Append(text);
        }

        return result.ToString();
    }

    private static bool NeedsSpaceBetween(
        TextItem prevItem,
        TextItem item,
        StringBuilder result,
        float singleCharThreshold)
    {
        var text = item.Text;

        // Hyphenated words keep no space around the hyphen.
        var prevEndsWithHyphen = result.Length > 0 && result[^1] == '-';
        var currIsHyphen = text.Trim() == "-";
        var currStartsWithHyphen = text.StartsWith('-');

        // Subscript and superscript: a smaller font with a vertical offset.
        var fontRatio = item.FontSize / prevItem.FontSize;
        var reverseFontRatio = prevItem.FontSize / item.FontSize;
        var yDiff = MathF.Abs(item.Y - prevItem.Y);

        var isSubSuper = fontRatio < 0.85f && yDiff > 1.0f;
        var wasSubSuper = reverseFontRatio < 0.85f && yDiff > 1.0f;

        var shouldJoin = TextUtils.ShouldJoinItems(prevItem, item, singleCharThreshold);

        var prevEndsWithSpace = result.Length > 0 && result[^1] == ' ';
        var currStartsWithSpace = text.StartsWith(' ');
        var spaceAlreadyExists = prevEndsWithSpace || currStartsWithSpace;

        return !(prevEndsWithHyphen
            || currIsHyphen
            || currStartsWithHyphen
            || isSubSuper
            || wasSubSuper
            || shouldJoin
            || spaceAlreadyExists);
    }
}
