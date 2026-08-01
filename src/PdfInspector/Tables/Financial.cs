// Ported from reference/src/tables/financial.rs
using PdfInspector.Types;

namespace PdfInspector.Tables;

/// <summary>
/// Splits a consolidated financial item into its individual values. Statement
/// PDFs often emit a whole row of figures as one text item, which column
/// detection cannot see through.
/// </summary>
internal static class Financial
{
    /// <summary>True when a token reads as a financial number: digits plus the usual punctuation.</summary>
    public static bool IsNumericToken(string token)
    {
        if (token.Length == 0)
        {
            return false;
        }

        var hasDigit = false;
        foreach (var c in token)
        {
            if (char.IsAsciiDigit(c))
            {
                hasDigit = true;
            }
            else if (c is not (',' or '.' or '(' or ')' or '-' or '+' or '%'))
            {
                return false;
            }
        }

        return hasDigit;
    }

    /// <summary>True for the dashes financial tables use as a nil marker.</summary>
    public static bool IsDashToken(string token) => token is "—" or "–" or "-" or "‒";

    /// <summary>
    /// True when the text holds two consecutive letters, which rules out a
    /// pure-value item such as "Land $ 778,177".
    /// </summary>
    public static bool HasAlphabeticWords(string text)
    {
        var consecutive = 0;
        foreach (var c in text)
        {
            if (char.IsLetter(c))
            {
                consecutive++;
                if (consecutive >= 2)
                {
                    return true;
                }
            }
            else
            {
                consecutive = 0;
            }
        }

        return false;
    }

    /// <summary>
    /// Groups whitespace-separated tokens into financial values: a currency sign
    /// binds to the number after it, and a bare number or dash stands alone. Any
    /// unrecognised token means the item is not a pure row of values.
    /// </summary>
    public static List<string>? TokenizeFinancialValues(string text)
    {
        var tokens = text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
        if (tokens.Length == 0)
        {
            return null;
        }

        var values = new List<string>();
        var i = 0;

        while (i < tokens.Length)
        {
            var token = tokens[i];

            if (token == "$")
            {
                if (i + 1 < tokens.Length && IsNumericToken(tokens[i + 1]))
                {
                    values.Add($"{token} {tokens[i + 1]}");
                    i += 2;
                }
                else
                {
                    return null;
                }
            }
            else if (IsNumericToken(token) || IsDashToken(token))
            {
                values.Add(token);
                i++;
            }
            else
            {
                return null;
            }
        }

        return values.Count == 0 ? null : values;
    }

    /// <summary>
    /// Splits a wide, alphabetic-free item that tokenizes into at least three
    /// values, spacing the sub-items evenly across the original's span.
    /// </summary>
    public static List<TextItem>? TrySplitFinancialItem(TextItem item)
    {
        if (item.Width <= item.FontSize * 20.0f)
        {
            return null;
        }

        if (HasAlphabeticWords(item.Text))
        {
            return null;
        }

        var values = TokenizeFinancialValues(item.Text);
        if (values is null || values.Count < 3)
        {
            return null;
        }

        var spacing = item.Width / values.Count;
        var subWidth = spacing * 0.9f;
        var subItems = new List<TextItem>(values.Count);

        for (var i = 0; i < values.Count; i++)
        {
            var sub = item.Clone();
            sub.Text = values[i];
            sub.X = item.X + (spacing * i) + (spacing * 0.5f);
            sub.Width = subWidth;
            subItems.Add(sub);
        }

        return subItems;
    }
}
