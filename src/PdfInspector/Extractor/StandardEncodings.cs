// Replaces the encoding tables the Rust build gets from lopdf.
namespace PdfInspector.Extractor;

/// <summary>
/// The predefined single-byte encodings of ISO 32000-1, annex D. Each is stored
/// as the glyph name for every code, which is how the specification defines
/// them; names resolve to characters through the Adobe Glyph List.
/// </summary>
internal static class StandardEncodings
{
    /// <summary>
    /// Codes 32–255 of StandardEncoding. Undefined codes are the empty string.
    /// </summary>
    private static readonly string[] StandardHigh =
    [
        /* 32 */ "space", "exclam", "quotedbl", "numbersign", "dollar", "percent",
        "ampersand", "quoteright", "parenleft", "parenright", "asterisk", "plus",
        "comma", "hyphen", "period", "slash",
        /* 48 */ "zero", "one", "two", "three", "four", "five", "six", "seven",
        "eight", "nine", "colon", "semicolon", "less", "equal", "greater", "question",
        /* 64 */ "at", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L",
        "M", "N", "O",
        /* 80 */ "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
        "bracketleft", "backslash", "bracketright", "asciicircum", "underscore",
        /* 96 */ "quoteleft", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k",
        "l", "m", "n", "o",
        /* 112 */ "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z",
        "braceleft", "bar", "braceright", "asciitilde", "",
        /* 128 */ "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
        /* 144 */ "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
        /* 160 */ "", "exclamdown", "cent", "sterling", "fraction", "yen",
        "florin", "section", "currency", "quotesingle", "quotedblleft",
        "guillemotleft", "guilsinglleft", "guilsinglright", "fi", "fl",
        /* 176 */ "", "endash", "dagger", "daggerdbl", "periodcentered", "",
        "paragraph", "bullet", "quotesinglbase", "quotedblbase", "quotedblright",
        "guillemotright", "ellipsis", "perthousand", "", "questiondown",
        /* 192 */ "", "grave", "acute", "circumflex", "tilde", "macron", "breve",
        "dotaccent", "dieresis", "", "ring", "cedilla", "", "hungarumlaut",
        "ogonek", "caron",
        /* 208 */ "emdash", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
        /* 224 */ "", "AE", "", "ordfeminine", "", "", "", "", "Lslash", "Oslash",
        "OE", "ordmasculine", "", "", "", "",
        /* 240 */ "", "ae", "", "", "", "dotlessi", "", "", "lslash", "oslash",
        "oe", "germandbls", "", "", "", "",
    ];

    /// <summary>Codes 32–255 of WinAnsiEncoding.</summary>
    private static readonly string[] WinAnsiHigh =
    [
        /* 32 */ "space", "exclam", "quotedbl", "numbersign", "dollar", "percent",
        "ampersand", "quotesingle", "parenleft", "parenright", "asterisk", "plus",
        "comma", "hyphen", "period", "slash",
        /* 48 */ "zero", "one", "two", "three", "four", "five", "six", "seven",
        "eight", "nine", "colon", "semicolon", "less", "equal", "greater", "question",
        /* 64 */ "at", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L",
        "M", "N", "O",
        /* 80 */ "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
        "bracketleft", "backslash", "bracketright", "asciicircum", "underscore",
        /* 96 */ "grave", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k",
        "l", "m", "n", "o",
        /* 112 */ "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z",
        "braceleft", "bar", "braceright", "asciitilde", "bullet",
        /* 128 */ "Euro", "bullet", "quotesinglbase", "florin", "quotedblbase",
        "ellipsis", "dagger", "daggerdbl", "circumflex", "perthousand", "Scaron",
        "guilsinglleft", "OE", "bullet", "Zcaron", "bullet",
        /* 144 */ "bullet", "quoteleft", "quoteright", "quotedblleft",
        "quotedblright", "bullet", "endash", "emdash", "tilde", "trademark",
        "scaron", "guilsinglright", "oe", "bullet", "zcaron", "Ydieresis",
        /* 160 */ "space", "exclamdown", "cent", "sterling", "currency", "yen",
        "brokenbar", "section", "dieresis", "copyright", "ordfeminine",
        "guillemotleft", "logicalnot", "hyphen", "registered", "macron",
        /* 176 */ "degree", "plusminus", "twosuperior", "threesuperior", "acute",
        "mu", "paragraph", "periodcentered", "cedilla", "onesuperior",
        "ordmasculine", "guillemotright", "onequarter", "onehalf",
        "threequarters", "questiondown",
        /* 192 */ "Agrave", "Aacute", "Acircumflex", "Atilde", "Adieresis",
        "Aring", "AE", "Ccedilla", "Egrave", "Eacute", "Ecircumflex", "Edieresis",
        "Igrave", "Iacute", "Icircumflex", "Idieresis",
        /* 208 */ "Eth", "Ntilde", "Ograve", "Oacute", "Ocircumflex", "Otilde",
        "Odieresis", "multiply", "Oslash", "Ugrave", "Uacute", "Ucircumflex",
        "Udieresis", "Yacute", "Thorn", "germandbls",
        /* 224 */ "agrave", "aacute", "acircumflex", "atilde", "adieresis",
        "aring", "ae", "ccedilla", "egrave", "eacute", "ecircumflex", "edieresis",
        "igrave", "iacute", "icircumflex", "idieresis",
        /* 240 */ "eth", "ntilde", "ograve", "oacute", "ocircumflex", "otilde",
        "odieresis", "divide", "oslash", "ugrave", "uacute", "ucircumflex",
        "udieresis", "yacute", "thorn", "ydieresis",
    ];

    /// <summary>Codes 32–255 of MacRomanEncoding.</summary>
    private static readonly string[] MacRomanHigh =
    [
        /* 32 */ "space", "exclam", "quotedbl", "numbersign", "dollar", "percent",
        "ampersand", "quotesingle", "parenleft", "parenright", "asterisk", "plus",
        "comma", "hyphen", "period", "slash",
        /* 48 */ "zero", "one", "two", "three", "four", "five", "six", "seven",
        "eight", "nine", "colon", "semicolon", "less", "equal", "greater", "question",
        /* 64 */ "at", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L",
        "M", "N", "O",
        /* 80 */ "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
        "bracketleft", "backslash", "bracketright", "asciicircum", "underscore",
        /* 96 */ "grave", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k",
        "l", "m", "n", "o",
        /* 112 */ "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z",
        "braceleft", "bar", "braceright", "asciitilde", "",
        /* 128 */ "Adieresis", "Aring", "Ccedilla", "Eacute", "Ntilde",
        "Odieresis", "Udieresis", "aacute", "agrave", "acircumflex", "adieresis",
        "atilde", "aring", "ccedilla", "eacute", "egrave",
        /* 144 */ "ecircumflex", "edieresis", "iacute", "igrave", "icircumflex",
        "idieresis", "ntilde", "oacute", "ograve", "ocircumflex", "odieresis",
        "otilde", "uacute", "ugrave", "ucircumflex", "udieresis",
        /* 160 */ "dagger", "degree", "cent", "sterling", "section", "bullet",
        "paragraph", "germandbls", "registered", "copyright", "trademark",
        "acute", "dieresis", "notequal", "AE", "Oslash",
        /* 176 */ "infinity", "plusminus", "lessequal", "greaterequal", "yen",
        "mu", "partialdiff", "summation", "product", "pi", "integral",
        "ordfeminine", "ordmasculine", "Omega", "ae", "oslash",
        /* 192 */ "questiondown", "exclamdown", "logicalnot", "radical", "florin",
        "approxequal", "Delta", "guillemotleft", "guillemotright", "ellipsis",
        "space", "Agrave", "Atilde", "Otilde", "OE", "oe",
        /* 208 */ "endash", "emdash", "quotedblleft", "quotedblright", "quoteleft",
        "quoteright", "divide", "lozenge", "ydieresis", "Ydieresis", "fraction",
        "currency", "guilsinglleft", "guilsinglright", "fi", "fl",
        /* 224 */ "daggerdbl", "periodcentered", "quotesinglbase", "quotedblbase",
        "perthousand", "Acircumflex", "Ecircumflex", "Aacute", "Edieresis",
        "Egrave", "Iacute", "Icircumflex", "Idieresis", "Igrave", "Oacute",
        "Ocircumflex",
        /* 240 */ "apple", "Ograve", "Uacute", "Ucircumflex", "Ugrave", "dotlessi",
        "circumflex", "tilde", "macron", "breve", "dotaccent", "ring", "cedilla",
        "hungarumlaut", "ogonek", "caron",
    ];

    private static readonly Lazy<char?[]> StandardTable = new(() => Build(StandardHigh));
    private static readonly Lazy<char?[]> WinAnsiTable = new(() => Build(WinAnsiHigh));
    private static readonly Lazy<char?[]> MacRomanTable = new(() => Build(MacRomanHigh));

    /// <summary>Resolves a base-encoding name to its 256-entry table, or null when unknown.</summary>
    public static char?[]? ByName(string name) => name switch
    {
        "WinAnsiEncoding" => WinAnsiTable.Value,
        "MacRomanEncoding" => MacRomanTable.Value,
        "StandardEncoding" or "PDFDocEncoding" => StandardTable.Value,
        // MacExpertEncoding covers an expert glyph set with no Unicode analogue
        // for most codes; the reference build treats it as Standard.
        "MacExpertEncoding" => StandardTable.Value,
        _ => null,
    };

    /// <summary>The default when a font declares no encoding.</summary>
    public static char?[] Standard => StandardTable.Value;

    public static char?[] WinAnsi => WinAnsiTable.Value;

    private static char?[] Build(string[] high)
    {
        var table = new char?[256];
        for (var i = 0; i < high.Length && 32 + i < 256; i++)
        {
            var name = high[i];
            if (name.Length == 0)
            {
                continue;
            }

            table[32 + i] = Text.GlyphNames.GlyphToChar(name);
        }

        return table;
    }
}
