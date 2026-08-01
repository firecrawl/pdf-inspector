// Ported from reference/src/text_utils.rs
using System.Text;
using PdfInspector.Types;

namespace PdfInspector.Text;

/// <summary>
/// Character classification and text helpers. Pure functions over characters,
/// strings, and <see cref="TextItem"/> collections — no PDF parsing happens here.
/// </summary>
internal static class TextUtils
{
    /// <summary>
    /// True for CJK characters. CJK languages do not use spaces between words,
    /// so word-boundary heuristics must not apply when they are involved.
    /// </summary>
    public static bool IsCjkChar(char c) =>
        c is >= '\u1100' and <= '\u11FF'    // Hangul Jamo
            or >= '\u3000' and <= '\u303F'  // CJK Symbols and Punctuation
            or >= '\u3040' and <= '\u309F'  // Hiragana
            or >= '\u30A0' and <= '\u30FF'  // Katakana
            or >= '\u3130' and <= '\u318F'  // Hangul Compatibility Jamo
            or >= '\u4E00' and <= '\u9FFF'  // CJK Unified Ideographs
            or >= '\uAC00' and <= '\uD7AF'  // Hangul Syllables
            or >= '\uF900' and <= '\uFAFF'  // CJK Compatibility Ideographs
            or >= '\uFF00' and <= '\uFFEF'; // Halfwidth and Fullwidth Forms

    public static bool IsRtlChar(char c) =>
        c is >= '\u0590' and <= '\u05FF'    // Hebrew
            or >= '\u0600' and <= '\u06FF'  // Arabic
            or >= '\u0700' and <= '\u074F'  // Syriac
            or >= '\u0750' and <= '\u077F'  // Arabic Supplement
            or >= '\u0780' and <= '\u07BF'  // Thaana
            or >= '\u07C0' and <= '\u07FF'  // NKo
            or >= '\u0800' and <= '\u083F'  // Samaritan
            or >= '\u0840' and <= '\u085F'  // Mandaic
            or >= '\u08A0' and <= '\u08FF'  // Arabic Extended-A
            or >= '\uFB1D' and <= '\uFB4F'  // Hebrew Presentation Forms
            or >= '\uFB50' and <= '\uFDFF'  // Arabic Presentation Forms-A
            or >= '\uFE70' and <= '\uFEFF'; // Arabic Presentation Forms-B

    /// <summary>
    /// U+FEFF is a byte-order mark rather than an Arabic presentation form,
    /// despite falling inside Presentation Forms-B.
    /// </summary>
    private static bool IsArabicPresentationForm(char c) =>
        c is >= '\uFB50' and <= '\uFDFF' or >= '\uFE70' and <= '\uFEFE';

    public static bool IsRtlText(IEnumerable<string> texts)
    {
        uint rtl = 0;
        uint ltr = 0;

        foreach (var text in texts)
        {
            foreach (var c in text)
            {
                if (IsRtlChar(c))
                {
                    rtl++;
                }
                else if (char.IsLetter(c) && !IsCjkChar(c))
                {
                    ltr++;
                }
            }
        }

        return rtl > 0 && rtl > ltr;
    }

    /// <summary>Sorts a line's items into reading order, right-to-left when the line is RTL.</summary>
    public static void SortLineItems(List<TextItem> items)
    {
        var rtl = IsRtlText(items.Select(i => i.Text));

        // A stable sort keeps equal-x items in extraction order, which the
        // reference build relies on for overlapping glyphs.
        var ordered = rtl
            ? items.OrderByDescending(i => i.X, FloatTotalOrder.Instance).ToList()
            : items.OrderBy(i => i.X, FloatTotalOrder.Instance).ToList();

        items.Clear();
        items.AddRange(ordered);
    }

    /// <summary>
    /// Detects a bold font from its name. Care is needed so that "Oblique" does
    /// not produce a false positive.
    /// </summary>
    public static bool IsBoldFont(string fontName)
    {
        var lower = fontName.ToLowerInvariant();

        return lower.Contains("bold", StringComparison.Ordinal)
            || lower.Contains("-bd", StringComparison.Ordinal)
            || lower.Contains("_bd", StringComparison.Ordinal)
            || lower.Contains("black", StringComparison.Ordinal)
            || lower.Contains("heavy", StringComparison.Ordinal)
            || lower.Contains("demibold", StringComparison.Ordinal)
            || lower.Contains("semibold", StringComparison.Ordinal)
            || lower.Contains("demi-bold", StringComparison.Ordinal)
            || lower.Contains("semi-bold", StringComparison.Ordinal)
            || lower.Contains("extrabold", StringComparison.Ordinal)
            || lower.Contains("ultrabold", StringComparison.Ordinal)
            // Some fonts use Medium for semi-bold.
            || (lower.Contains("medium", StringComparison.Ordinal) && !lower.Contains("mediumitalic", StringComparison.Ordinal))
            // URW Type 1 fonts abbreviate Medium as "Medi" (e.g. NimbusRomNo9L-Medi,
            // the Times-Bold substitute in LaTeX documents; -MediItal is bold italic).
            || (lower.Contains("-medi", StringComparison.Ordinal) && !lower.Contains("mediumital", StringComparison.Ordinal));
    }

    /// <summary>Detects an italic or oblique font from its name.</summary>
    public static bool IsItalicFont(string fontName)
    {
        var lower = fontName.ToLowerInvariant();

        return lower.Contains("italic", StringComparison.Ordinal)
            || lower.Contains("oblique", StringComparison.Ordinal)
            || lower.Contains("-it", StringComparison.Ordinal)
            || lower.Contains("_it", StringComparison.Ordinal)
            || lower.Contains("slant", StringComparison.Ordinal)
            || lower.Contains("inclined", StringComparison.Ordinal)
            || lower.Contains("kursiv", StringComparison.Ordinal); // German for italic
    }

    /// <summary>
    /// Expands Unicode ligatures to their component characters, strips invisible
    /// characters, and restores logical order for visually-ordered Arabic.
    /// </summary>
    public static string ExpandLigatures(string text)
    {
        // Strip control characters other than newline, carriage return, and tab.
        var hasControl = false;
        foreach (var c in text)
        {
            if (c < 0x20 && c != '\n' && c != '\r' && c != '\t')
            {
                hasControl = true;
                break;
            }
        }

        if (hasControl)
        {
            var filtered = new StringBuilder(text.Length);
            foreach (var c in text)
            {
                if (c >= ' ' || c == '\n' || c == '\r' || c == '\t')
                {
                    filtered.Append(c);
                }
            }

            text = filtered.ToString();
        }

        // Arabic presentation forms signal visual-order storage that needs
        // reversing after normalisation.
        var hadPresentationForms = false;
        foreach (var c in text)
        {
            if (IsArabicPresentationForm(c))
            {
                hadPresentationForms = true;
                break;
            }
        }

        // NFKC runs only when presentation forms are present. Applying it to all
        // non-ASCII text would fold NBSP (U+00A0) into a regular space and break
        // downstream spacing logic. Latin ligatures are handled explicitly below.
        if (hadPresentationForms)
        {
            try
            {
                text = text.Normalize(NormalizationForm.FormKC);
            }
            catch (ArgumentException)
            {
                // Invalid surrogate pairs cannot be normalised; keep the original.
            }
        }

        var result = new StringBuilder(text.Length);
        foreach (var ch in text)
        {
            switch (ch)
            {
                // Explicit expansion still covers fonts that bypass NFKC, such as
                // custom ToUnicode mappings into the private-use area.
                case '\uFB00': result.Append("ff"); break;
                case '\uFB01': result.Append("fi"); break;
                case '\uFB02': result.Append("fl"); break;
                case '\uFB03': result.Append("ffi"); break;
                case '\uFB04': result.Append("ffl"); break;
                case '\uFB05':
                case '\uFB06': result.Append("st"); break;

                // Invisible characters that would otherwise pollute the markdown.
                case '\u00AD': break;                   // soft hyphen
                case '\u200B': break;                   // zero-width space
                case '\uFEFF': break;                   // BOM / zero-width no-break space
                case '\u200C':
                case '\u200D': break;                   // ZWNJ / ZWJ
                case '\u2060': break;                   // word joiner

                // Typographic spaces become ASCII so the spacing heuristics can see
                // word boundaries. NBSP (U+00A0) is deliberately excluded: it is
                // common in PDFs and the coordinate-based logic already handles it.
                case >= '\u2000' and <= '\u200A': result.Append(' '); break;

                default: result.Append(ch); break;
            }
        }

        var output = result.ToString();
        return hadPresentationForms ? ReverseVisualArabic(output) : output;
    }

    /// <summary>
    /// Reverses visual-order Arabic into logical order. Pure RTL text is simply
    /// reversed; mixed content splits into LTR and non-LTR runs, reverses the run
    /// order, and reverses only the non-LTR runs internally.
    /// </summary>
    private static string ReverseVisualArabic(string text)
    {
        var hasLtr = false;
        foreach (var c in text)
        {
            if (char.IsAsciiLetterOrDigit(c))
            {
                hasLtr = true;
                break;
            }
        }

        if (!hasLtr)
        {
            var reversed = text.ToCharArray();
            Array.Reverse(reversed);
            return new string(reversed);
        }

        var chars = text.ToCharArray();
        var runs = new List<(bool IsLtr, string Content)>();

        var i = 0;
        while (i < chars.Length)
        {
            var isLtr = IsLtrAt(chars, i);
            var run = new StringBuilder();
            while (i < chars.Length && IsLtrAt(chars, i) == isLtr)
            {
                run.Append(chars[i]);
                i++;
            }

            runs.Add((isLtr, run.ToString()));
        }

        runs.Reverse();
        var result = new StringBuilder(text.Length);
        foreach (var (isLtr, content) in runs)
        {
            if (isLtr)
            {
                result.Append(content);
            }
            else
            {
                for (var j = content.Length - 1; j >= 0; j--)
                {
                    result.Append(content[j]);
                }
            }
        }

        return result.ToString();
    }

    private static bool IsLtrAt(char[] chars, int index)
    {
        var c = chars[index];
        return char.IsAsciiLetterOrDigit(c) || (IsAsciiPunctuation(c) && IsAdjacentToAsciiAlnum(chars, index));
    }

    private static bool IsAdjacentToAsciiAlnum(char[] chars, int index) =>
        (index > 0 && char.IsAsciiLetterOrDigit(chars[index - 1]))
        || (index + 1 < chars.Length && char.IsAsciiLetterOrDigit(chars[index + 1]));

    /// <summary>Matches Rust's <c>char::is_ascii_punctuation</c>.</summary>
    public static bool IsAsciiPunctuation(char c) =>
        c is >= '!' and <= '/' or >= ':' and <= '@' or >= '[' and <= '`' or >= '{' and <= '~';

    /// <summary>
    /// Decodes a PDF text string (ActualText and similar) that may be UTF-16BE
    /// with a byte-order mark, or PDFDocEncoding.
    /// </summary>
    public static string DecodeTextString(ReadOnlySpan<byte> bytes)
    {
        if (bytes.Length >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF)
        {
            var body = bytes[2..];
            var units = body.Length / 2;
            var sb = new StringBuilder(units);
            for (var i = 0; i < units; i++)
            {
                sb.Append((char)((body[i * 2] << 8) | body[(i * 2) + 1]));
            }

            return sb.ToString();
        }

        // PDFDocEncoding is a Latin-1 superset over the range that matters here.
        var latin = new StringBuilder(bytes.Length);
        foreach (var b in bytes)
        {
            latin.Append((char)b);
        }

        return latin.ToString();
    }

    /// <summary>
    /// Computes the effective font size from the base size and text matrix
    /// <c>[a, b, c, d, tx, ty]</c>, taking the larger of the two axis scales.
    /// </summary>
    public static float EffectiveFontSize(float baseSize, ReadOnlySpan<float> textMatrix)
    {
        var scaleX = MathF.Sqrt((textMatrix[0] * textMatrix[0]) + (textMatrix[1] * textMatrix[1]));
        var scaleY = MathF.Sqrt((textMatrix[2] * textMatrix[2]) + (textMatrix[3] * textMatrix[3]));
        return baseSize * MathF.Max(scaleX, scaleY);
    }

    /// <summary>Item width, falling back to a character-count estimate when the measured width is zero.</summary>
    public static float EffectiveWidth(TextItem item) =>
        item.Width > 0.0f ? item.Width : CharCount(item.Text) * item.FontSize * 0.5f;

    public static bool IsCidFont(string font) =>
        font.StartsWith("C2_", StringComparison.Ordinal) || font.StartsWith("C0_", StringComparison.Ordinal);

    /// <summary>
    /// The UTF-8 byte length of a string, matching Rust's <c>str::len()</c>.
    /// </summary>
    /// <remarks>
    /// The reference compares text lengths against tuned thresholds using byte
    /// length, so a non-ASCII cell counts for more there than its character count
    /// suggests. Using the character count instead would silently shift every one
    /// of those thresholds on non-ASCII text.
    /// </remarks>
    public static int ByteLength(string text) => System.Text.Encoding.UTF8.GetByteCount(text);

    /// <summary>Counts Unicode scalar values, matching Rust's <c>chars().count()</c>.</summary>
    public static int CharCount(string text)
    {
        var count = 0;
        for (var i = 0; i < text.Length; i++)
        {
            count++;
            if (char.IsHighSurrogate(text[i]) && i + 1 < text.Length && char.IsLowSurrogate(text[i + 1]))
            {
                i++;
            }
        }

        return count;
    }

    /// <summary>Last character of a string, or null when it is empty.</summary>
    public static char? LastChar(string text) => text.Length == 0 ? null : text[^1];

    /// <summary>First character of a string, or null when it is empty.</summary>
    public static char? FirstChar(string text) => text.Length == 0 ? null : text[0];

    // ── Letter-spacing detection ─────────────────────────────────────────

    /// <summary>The join threshold used for ordinary pages.</summary>
    public const float DefaultJoinThreshold = 0.10f;

    /// <summary>
    /// Detects and repairs Canva-style letter-spacing, where text is rendered
    /// character-by-character and the TJ handler ends up producing "a r i b"
    /// instead of "arib". Only activates when at least half the page's items are
    /// affected, so ordinary pages with short items are not disturbed.
    /// </summary>
    /// <returns>
    /// The adaptive join threshold for this page: <see cref="DefaultJoinThreshold"/>
    /// for normal pages, or a higher derived threshold for letter-spaced pages.
    /// </returns>
    public static float FixLetterspacedItems(List<TextItem> items)
    {
        if (items.Count == 0)
        {
            return DefaultJoinThreshold;
        }

        uint letterspacedCount = 0;
        uint totalTextItems = 0;
        foreach (var item in items)
        {
            var trimmed = item.Text.Trim();
            // The reference build compares UTF-8 byte length here, which only
            // matters for items shorter than three bytes.
            if (trimmed.Length == 0 || Encoding.UTF8.GetByteCount(trimmed) < 3)
            {
                continue;
            }

            totalTextItems++;
            if (IsLetterspaced(item.Text))
            {
                letterspacedCount++;
            }
        }

        if (totalTextItems < 4 || letterspacedCount * 2 < totalTextItems)
        {
            // Second path: per-character rendering with no embedded spaces, where
            // each character arrives as its own item.
            var singleCharCount = items.Count(i => CharCount(i.Text.Trim()) == 1);
            if (items.Count >= 10 && singleCharCount * 2 >= items.Count)
            {
                var canvaThreshold = ComputeCanvaJoinThreshold(items);
                if (canvaThreshold > 0.40f)
                {
                    return canvaThreshold;
                }
            }

            return DefaultJoinThreshold;
        }

        // The threshold is computed before the spaces are removed. This page is
        // confirmed letter-spaced, so the ungated variant is used — the
        // char-count guard would discard long items such as "i s s i o n".
        var threshold = ComputeCanvaJoinThreshold(items);

        foreach (var item in items)
        {
            if (IsLetterspaced(item.Text))
            {
                item.Text = item.Text.Replace(" ", string.Empty, StringComparison.Ordinal);
            }
        }

        return threshold;
    }

    /// <summary>True when the text alternates single characters and spaces.</summary>
    private static bool IsLetterspaced(string text)
    {
        var trimmed = text.Trim();
        if (CharCount(trimmed) < 3)
        {
            return false;
        }

        for (var i = 0; i < trimmed.Length; i++)
        {
            var isSpace = trimmed[i] == ' ';
            if (i % 2 == 0 ? isSpace : !isSpace)
            {
                return false;
            }
        }

        return true;
    }

    /// <summary>
    /// Computes the join threshold for a confirmed letter-spaced page from the
    /// median gap ratio. Single-character pairs use character-width joining in
    /// <see cref="ShouldJoinItems"/> instead of this page-level value.
    /// </summary>
    private static float ComputeCanvaJoinThreshold(List<TextItem> items)
    {
        const int MinSamples = 8;

        var ratios = CollectGapRatios(items);
        if (ratios.Count < MinSamples)
        {
            return DefaultJoinThreshold;
        }

        ratios.Sort();

        if (ratios[^1] < 0.40f || ratios[0] < 0.40f)
        {
            return DefaultJoinThreshold;
        }

        var median = ratios[ratios.Count / 2];
        return Math.Clamp(median * 1.55f, 0.50f, 2.0f);
    }

    /// <summary>
    /// Collects positive gap/font-size ratios from adjacent pairs, discarding CJK,
    /// zero-width, and out-of-range values.
    /// </summary>
    private static List<float> CollectGapRatios(List<TextItem> items)
    {
        var ratios = new List<float>();

        for (var i = 0; i + 1 < items.Count; i++)
        {
            var prev = items[i];
            var curr = items[i + 1];

            var prevChar = LastChar(prev.Text.Trim());
            var currChar = FirstChar(curr.Text.Trim());
            if ((prevChar is not null && IsCjkChar(prevChar.Value)) ||
                (currChar is not null && IsCjkChar(currChar.Value)))
            {
                continue;
            }

            if (prev.Width <= 0.0f || prev.FontSize <= 0.0f)
            {
                continue;
            }

            var gap = prev.X <= curr.X
                ? curr.X - (prev.X + prev.Width)
                : prev.X - (curr.X + curr.Width);

            var ratio = gap / prev.FontSize;
            if (ratio is >= 0.0f and <= 3.0f)
            {
                ratios.Add(ratio);
            }
        }

        return ratios;
    }

    // ── Item joining ─────────────────────────────────────────────────────

    /// <summary>
    /// Decides whether two adjacent text items should be joined without a space,
    /// from their positions on the page and the characters at the junction.
    /// </summary>
    public static bool ShouldJoinItems(TextItem prevItem, TextItem currItem, float singleCharThreshold)
    {
        // Explicit leading or trailing spaces are authoritative.
        if (prevItem.Text.EndsWith(' ') || currItem.Text.StartsWith(' '))
        {
            return false;
        }

        var prevLast = LastChar(prevItem.Text.TrimEnd());
        var currFirst = FirstChar(currItem.Text.TrimStart());

        // Punctuation that conventionally follows without a space: "www" + ".com".
        if (currFirst is '.' or ',' or ';' or '!' or '?' or ')' or ']' or '}' or '\'')
        {
            return true;
        }

        // A colon followed by alphanumerics is a label/value pair and keeps its space.
        if (prevLast == ':' && currFirst is not null && char.IsLetterOrDigit(currFirst.Value))
        {
            return false;
        }

        if (prevItem.Width > 0.0f)
        {
            var gap = prevItem.X <= currItem.X
                ? currItem.X - (prevItem.X + prevItem.Width)   // LTR: prev is left of curr
                : prevItem.X - (currItem.X + currItem.Width);  // RTL: prev is right of curr

            var fontSize = prevItem.FontSize;

            // Never join across column-scale gaps or large overlaps. Large negative
            // gaps arise when Tc/Tw inflate item widths past where the next starts.
            if (gap > fontSize * 3.0f || gap < -fontSize)
            {
                return false;
            }

            var prevChars = CharCount(prevItem.Text.Trim());
            var currChars = CharCount(currItem.Text.Trim());
            var prevLastChar = LastChar(prevItem.Text.Trim());
            var currFirstChar = FirstChar(currItem.Text.Trim());
            var isCjk = (prevLastChar is not null && IsCjkChar(prevLastChar.Value))
                || (currFirstChar is not null && IsCjkChar(currFirstChar.Value));

            // CID fonts emit one word per text operator with gaps near zero, so
            // those boundaries need a space. Non-CID fonts emit phrases and must
            // not trigger this.
            if (!isCjk && gap >= 0.0f && gap < fontSize * 0.01f && IsCidFont(prevItem.Font))
            {
                var prevWordCount = prevItem.Text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).Length;

                if (prevWordCount >= 3)
                {
                    // A multi-word phrase from a line-level CID operator — most
                    // likely a mid-word boundary.
                    return gap < fontSize * 0.15f;
                }

                return false;
            }

            // Digits, commas, periods, and percent signs positioned close together
            // are almost always one number: "34,20" + "8" → "34,208".
            if (prevLast is not null && currFirst is not null)
            {
                var p = prevLast.Value;
                var c = currFirst.Value;

                var prevIsNumeric = char.IsAsciiDigit(p) || p == ',' || p == '.';
                var currIsNumeric = char.IsAsciiDigit(c) || c == '%' || c == '.';
                if (prevIsNumeric && currIsNumeric)
                {
                    return gap > -fontSize && gap < fontSize * 0.3f;
                }

                if ((p == '+' || p == '-') && char.IsAsciiDigit(c))
                {
                    return gap > -fontSize && gap < fontSize * 0.3f;
                }
            }

            // On a letter-spaced page, join by character width rather than font
            // size: for single-character items the rendered width is an accurate
            // reference, and letter gaps sit near 1.0× it while word gaps exceed 1.5×.
            if (singleCharThreshold > 0.20f)
            {
                if (prevChars == 1)
                {
                    return gap < prevItem.Width * 1.25f;
                }

                if (currChars == 1)
                {
                    // Average character width normalises for a wide/narrow mix.
                    var avgCharWidth = prevItem.Width / prevChars;
                    return gap < avgCharWidth * 1.25f;
                }

                return gap < fontSize * singleCharThreshold;
            }

            // A single-character fragment against a multi-character item: rejoin
            // split words such as "b" + "illion" or "C" + "ultural".
            if ((prevChars == 1) != (currChars == 1))
            {
                return gap < fontSize * 0.20f;
            }

            // Both single-character: per-glyph positioning. Numeric junctions get a
            // generous threshold; alphabetic ones stay tight so word boundaries in
            // per-character PDFs are detected reliably.
            if (prevChars == 1 && currChars == 1)
            {
                if (prevLast is not null && currFirst is not null)
                {
                    var p = prevLast.Value;
                    var c = currFirst.Value;
                    var pNumeric = char.IsAsciiDigit(p) || p is ',' or '.' or '%' or '+' or '-';
                    var cNumeric = char.IsAsciiDigit(c) || c is ',' or '.' or '%';
                    if (pNumeric && cNumeric)
                    {
                        return gap < fontSize * 0.25f;
                    }
                }

                return gap < fontSize * singleCharThreshold;
            }

            // A lowercase→lowercase junction between multi-character items gets a
            // slightly wider threshold, so imprecise CID metrics do not split
            // "enterta"+"inment". Caps junctions keep the tighter value so that
            // "LCOE"+"WITH" stays two words.
            if (CharCount(prevItem.Text.Trim()) >= 2 && CharCount(currItem.Text.Trim()) >= 2)
            {
                var prevEndsLower = LastChar(prevItem.Text.Trim()) is { } pl && char.IsLower(pl);
                var currStartsLower = FirstChar(currItem.Text.Trim()) is { } cf && char.IsLower(cf);
                if (prevEndsLower && currStartsLower)
                {
                    return gap < fontSize * 0.18f;
                }
            }

            return gap < fontSize * 0.15f;
        }

        // Fallback: estimate width from the font size.
        var charWidth = prevItem.FontSize * 0.45f;
        var estimatedPrevWidth = CharCount(prevItem.Text) * charWidth;
        var prevEndX = prevItem.X + estimatedPrevWidth;
        var fallbackGap = currItem.X - prevEndX;

        if (fallbackGap > charWidth * 6.0f)
        {
            return false;
        }

        // CJK never uses inter-word spaces, so the case heuristics below would
        // wrongly split words.
        var fallbackCjk = (prevLast is not null && IsCjkChar(prevLast.Value))
            || (currFirst is not null && IsCjkChar(currFirst.Value));
        if (fallbackCjk)
        {
            return fallbackGap < charWidth * 0.8f;
        }

        if (prevLast is not null && currFirst is not null &&
            char.IsLetter(prevLast.Value) && char.IsLetter(currFirst.Value))
        {
            var p = prevLast.Value;
            var c = currFirst.Value;
            var sameCase = (char.IsUpper(p) && char.IsUpper(c)) || (char.IsLower(p) && char.IsLower(c));

            if (sameCase)
            {
                // Likely a split fragment of one word: "CONST" + "ANCIA".
                return fallbackGap < charWidth * 0.8f;
            }

            if (char.IsLower(p) && char.IsUpper(c))
            {
                // Words do not transition lowercase→uppercase mid-word, so this is
                // always a boundary regardless of position.
                return false;
            }

            // Uppercase→lowercase, e.g. "REGISTRO" + "para": likely a boundary.
            return fallbackGap < charWidth * 0.3f;
        }

        return fallbackGap < charWidth * 0.5f;
    }
}

/// <summary>
/// Orders floats the way Rust's <c>f32::total_cmp</c> does, so NaN and signed
/// zero sort deterministically rather than being treated as unordered.
/// </summary>
internal sealed class FloatTotalOrder : IComparer<float>
{
    public static readonly FloatTotalOrder Instance = new();

    public int Compare(float x, float y)
    {
        var left = BitConverter.SingleToInt32Bits(x);
        var right = BitConverter.SingleToInt32Bits(y);

        // Flip the ordering of negative values so the bit patterns compare in
        // numeric order.
        left ^= (int)((uint)(left >> 31) >> 1);
        right ^= (int)((uint)(right >> 31) >> 1);

        return left.CompareTo(right);
    }
}
