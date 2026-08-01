// Ported from reference/src/text_quality.rs
namespace PdfInspector.Quality;

/// <summary>
/// Letter statistics for detecting substitution-cipher garbling: a broken
/// ToUnicode CMap that shifts every character by a per-range constant, so
/// "Certificate" extracts as "8VceZWZTReV". Such text is entirely printable
/// ASCII with word-like token lengths, produces no replacement characters, and
/// therefore defeats the other detectors — it needs its own discriminator.
/// </summary>
internal sealed class CipherGarbleStats
{
    /// <summary>
    /// English letter frequencies (percent, a–z), used as a natural-language
    /// reference. Every Latin-script language in the reference corpus (Swedish,
    /// Finnish, Turkish, German, romaji) scores at or above 0.80 cosine
    /// similarity against it, while substitution-cipher text scores about 0.53.
    /// </summary>
    private static readonly double[] EnglishLetterFrequency =
    [
        8.2, 1.5, 2.8, 4.3, 12.7, 2.2, 2.0, 6.1, 7.0, 0.15, 0.8, 4.0, 2.4,
        6.7, 7.5, 1.9, 0.1, 6.0, 6.3, 9.1, 2.8, 1.0, 2.4, 0.15, 2.0, 0.07,
    ];

    private static readonly double[] EnglishSortedDescending = BuildSortedEnglish();

    private static readonly double EnglishNorm =
        Math.Sqrt(EnglishLetterFrequency.Sum(f => f * f));

    private static double[] BuildSortedEnglish()
    {
        var sorted = (double[])EnglishLetterFrequency.Clone();
        Array.Sort(sorted);
        Array.Reverse(sorted);
        return sorted;
    }

    /// <summary>Case-folded ASCII letter histogram.</summary>
    private readonly uint[] _letterCounts = new uint[26];

    private int _asciiLetters;
    private int _asciiVowels;

    /// <summary>
    /// Accented Latin letters (Latin-1 Supplement through Latin Extended-B, plus
    /// Latin Extended Additional). These count toward Latin dominance only.
    /// </summary>
    private int _latinExtLetters;

    private int _nonLatinLetters;

    /// <summary>Adjacent ASCII-letter pairs, and how many switch from lowercase straight to uppercase.</summary>
    private int _letterBigrams;

    private int _caseShiftBigrams;

    public void AddText(string text)
    {
        char? prev = null;

        foreach (var ch in text)
        {
            if (char.IsAsciiLetter(ch))
            {
                var lower = char.ToLowerInvariant(ch);
                _letterCounts[lower - 'a']++;
                _asciiLetters++;

                if (lower is 'a' or 'e' or 'i' or 'o' or 'u')
                {
                    _asciiVowels++;
                }

                if (prev is { } p)
                {
                    _letterBigrams++;
                    if (char.IsAsciiLetterLower(p) && char.IsAsciiLetterUpper(ch))
                    {
                        _caseShiftBigrams++;
                    }
                }

                prev = ch;
            }
            else
            {
                if (char.IsLetter(ch))
                {
                    // Latin-1 Supplement through Latin Extended-B, plus Latin
                    // Extended Additional.
                    if (ch is >= '\u00C0' and <= '\u024F' or >= '\u1E00' and <= '\u1EFF')
                    {
                        _latinExtLetters++;
                    }
                    else
                    {
                        _nonLatinLetters++;
                    }
                }

                prev = null;
            }
        }
    }

    /// <summary>
    /// Cosine similarity between the observed histogram and English letter
    /// frequencies. A shifted alphabet permutes the histogram, which destroys
    /// the similarity regardless of the shift amount.
    /// </summary>
    private double EnglishCosine()
    {
        if (_asciiLetters == 0)
        {
            return 1.0;
        }

        double n = _asciiLetters;
        var dot = 0.0;
        var normObserved = 0.0;

        for (var i = 0; i < 26; i++)
        {
            var p = _letterCounts[i] / n;
            dot += p * EnglishLetterFrequency[i];
            normObserved += p * p;
        }

        return dot / (Math.Sqrt(normObserved) * EnglishNorm);
    }

    /// <summary>
    /// Cosine similarity after sorting both histograms descending — comparing
    /// the shape of the frequency profile while ignoring which letter sits
    /// where. A substitution cipher is a bijection, so it preserves that shape
    /// exactly regardless of case or offset. Non-linguistic ASCII has a
    /// different profile: a small alphabet is far steeper, so the shape diverges.
    /// </summary>
    private double EnglishShapeCosine()
    {
        if (_asciiLetters == 0)
        {
            return 1.0;
        }

        double n = _asciiLetters;
        var observed = new double[26];
        for (var i = 0; i < 26; i++)
        {
            observed[i] = _letterCounts[i] / n;
        }

        Array.Sort(observed);
        Array.Reverse(observed);

        var dot = 0.0;
        var normObserved = 0.0;
        var normEnglish = 0.0;

        for (var i = 0; i < 26; i++)
        {
            dot += observed[i] * EnglishSortedDescending[i];
            normObserved += observed[i] * observed[i];
            normEnglish += EnglishSortedDescending[i] * EnglishSortedDescending[i];
        }

        return dot / (Math.Sqrt(normObserved) * Math.Sqrt(normEnglish));
    }

    /// <summary>
    /// Thresholds validated against the reference build's 380-document snapshot
    /// corpus (zero false positives) and its garbled benchmark page (vowel ratio
    /// 0.245, case-shift rate 0.225, cosine 0.532). The closest legitimate
    /// document on each axis: vowel ratio 0.264 (a circuit schematic), case-shift
    /// rate 0.021, cosine 0.801.
    /// </summary>
    public bool LooksGarbled()
    {
        // A statistically meaningful, Latin-dominant sample is required.
        if (_asciiLetters < 200 || _nonLatinLetters > _asciiLetters + _latinExtLetters)
        {
            return false;
        }

        // Real Latin-script text keeps vowels above roughly 30% of letters even
        // in acronym- and part-number-heavy documents; shifted text starves them.
        var vowelRatio = (double)_asciiVowels / _asciiLetters;
        if (vowelRatio > 0.30)
        {
            return false;
        }

        // Signal 1: lowercase→uppercase transitions inside words. A shifted
        // lowercase alphabet straddles the ASCII uppercase block ('i'→'Z',
        // 't'→'e'), so garbled words flip case constantly. Real documents stay
        // at or below 0.02 even with camelCase identifiers.
        var caseShifts = _letterBigrams >= 100 && _caseShiftBigrams >= _letterBigrams * 0.10;

        // Signal 2: the histogram is a permutation of natural language — an
        // English-like frequency shape (sorted cosine high) with the letters in
        // the wrong positions (unsorted cosine low). This is the signature of a
        // substitution cipher and is case-independent, so it catches
        // all-lowercase and all-uppercase shifts as well as case-straddling
        // ones. Genuinely non-linguistic ASCII that is merely "unlike English"
        // fails one half or the other: DNA and hex dumps have too steep a
        // profile, while protein sequences, ticker symbols, and base64 are not
        // sufficiently unlike English in position.
        var permutedLanguage = EnglishCosine() < 0.60 && EnglishShapeCosine() >= 0.90;

        return caseShifts || permutedLanguage;
    }
}
