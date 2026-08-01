// Ported from reference/src/structure_tree.rs
using System.Text;

namespace PdfInspector.Structure;

/// <summary>
/// Repairs malformed structure-element <c>/S</c> entries in raw PDF bytes.
/// Some producers (notably fpdf2) write a bare token — <c>/S Code</c> — where a
/// name is required. A strict parser drops the whole object, taking the
/// structure tree with it, so the bytes are patched before parsing.
/// </summary>
internal static class BareStructNames
{
    private const string Module = "structure";

    /// <summary>
    /// Only valid structure types are patched, so an arbitrary dictionary value
    /// that happens to follow <c>/S</c> is left alone.
    /// </summary>
    private static readonly string[] KnownNames =
    [
        "Document", "Part", "Art", "Sect", "Div", "BlockQuote", "Caption",
        "TOC", "TOCI", "Index", "NonStruct", "Private",
        "H", "H1", "H2", "H3", "H4", "H5", "H6", "P",
        "L", "LI", "Lbl", "LBody",
        "Table", "TR", "TH", "TD", "THead", "TBody", "TFoot",
        "Span", "Quote", "Note", "Reference", "BibEntry", "Code", "Link", "Annot",
        "Figure", "Formula", "Form",
        "Ruby", "RB", "RT", "RP", "Warichu", "WT", "WP",
    ];

    private static readonly byte[][] KnownNameBytes =
        [.. KnownNames.Select(Encoding.ASCII.GetBytes)];

    /// <summary>
    /// Returns the patched bytes, or the original array when nothing needed fixing.
    /// </summary>
    public static byte[] Fix(byte[] buffer)
    {
        // Untagged files have nothing to repair.
        if (IndexOf(buffer, "/StructTreeRoot"u8, 0) < 0)
        {
            return buffer;
        }

        ReadOnlySpan<byte> pattern = "/S "u8;
        MemoryStream? result = null;
        var copied = 0;
        var pos = 0;

        while (pos + pattern.Length < buffer.Length)
        {
            var index = IndexOf(buffer, pattern, pos);
            if (index < 0)
            {
                break;
            }

            var after = index + pattern.Length;

            // Already a proper name.
            if (after < buffer.Length && buffer[after] == (byte)'/')
            {
                pos = after;
                continue;
            }

            var matched = false;
            foreach (var name in KnownNameBytes)
            {
                var end = after + name.Length;
                if (end > buffer.Length || !buffer.AsSpan(after, name.Length).SequenceEqual(name))
                {
                    continue;
                }

                // The name must be followed by a delimiter, so "TR" does not
                // match the leading characters of a longer bare token.
                if (end < buffer.Length && buffer[end] is not ((byte)'\n' or (byte)'\r' or (byte)' ' or (byte)'/' or (byte)'>'))
                {
                    continue;
                }

                result ??= new MemoryStream(buffer.Length + 64);
                result.Write(buffer, copied, after - copied);
                result.WriteByte((byte)'/');
                result.Write(name, 0, name.Length);
                copied = end;
                pos = end;
                matched = true;

                Log.Debug(Module, () =>
                {
                    var text = Encoding.ASCII.GetString(name);
                    return $"fix_bare_struct_names: patched /S {text} → /S /{text}";
                });

                break;
            }

            if (!matched)
            {
                pos = after;
            }
        }

        if (result is null)
        {
            return buffer;
        }

        result.Write(buffer, copied, buffer.Length - copied);
        return result.ToArray();
    }

    private static int IndexOf(byte[] haystack, ReadOnlySpan<byte> needle, int start)
    {
        if (start >= haystack.Length)
        {
            return -1;
        }

        var index = haystack.AsSpan(start).IndexOf(needle);
        return index < 0 ? -1 : index + start;
    }
}
