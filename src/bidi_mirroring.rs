//! `Bidi_Mirroring_Glyph`: the character whose glyph depicts a mirrored
//! character at an odd level under rule L4 of the Unicode Bidirectional
//! Algorithm. A line stored in display order holds these images — a
//! right-to-left run's `(` is painted as `)` — and reading the line back
//! turns them into the characters that were written.
//!
//! The table is `BidiMirroring.txt` of the Unicode Character Database,
//! version 18.0.0 (© 2026 Unicode, Inc.; used under the Unicode License v3,
//! <https://www.unicode.org/license.txt>): one entry per `source; image #
//! name` line of the file, sorted by the source character. To take up a
//! newer version of the file, rewrite the entries from its lines the same
//! way; `table_is_sorted_and_symmetric` checks the result.

/// The mirror image of `c`, or `None` when `c` has none.
pub(crate) fn mirrored(c: char) -> Option<char> {
    MIRRORING
        .binary_search_by_key(&c, |&(source, _)| source)
        .ok()
        .map(|index| MIRRORING[index].1)
}

/// (character, its mirror image), sorted by character.
const MIRRORING: &[(char, char)] = &[
    ('\u{0028}', '\u{0029}'),   // LEFT PARENTHESIS
    ('\u{0029}', '\u{0028}'),   // RIGHT PARENTHESIS
    ('\u{003C}', '\u{003E}'),   // LESS-THAN SIGN
    ('\u{003E}', '\u{003C}'),   // GREATER-THAN SIGN
    ('\u{005B}', '\u{005D}'),   // LEFT SQUARE BRACKET
    ('\u{005D}', '\u{005B}'),   // RIGHT SQUARE BRACKET
    ('\u{007B}', '\u{007D}'),   // LEFT CURLY BRACKET
    ('\u{007D}', '\u{007B}'),   // RIGHT CURLY BRACKET
    ('\u{00AB}', '\u{00BB}'),   // LEFT-POINTING DOUBLE ANGLE QUOTATION MARK
    ('\u{00BB}', '\u{00AB}'),   // RIGHT-POINTING DOUBLE ANGLE QUOTATION MARK
    ('\u{0F3A}', '\u{0F3B}'),   // TIBETAN MARK GUG RTAGS GYON
    ('\u{0F3B}', '\u{0F3A}'),   // TIBETAN MARK GUG RTAGS GYAS
    ('\u{0F3C}', '\u{0F3D}'),   // TIBETAN MARK ANG KHANG GYON
    ('\u{0F3D}', '\u{0F3C}'),   // TIBETAN MARK ANG KHANG GYAS
    ('\u{169B}', '\u{169C}'),   // OGHAM FEATHER MARK
    ('\u{169C}', '\u{169B}'),   // OGHAM REVERSED FEATHER MARK
    ('\u{2039}', '\u{203A}'),   // SINGLE LEFT-POINTING ANGLE QUOTATION MARK
    ('\u{203A}', '\u{2039}'),   // SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
    ('\u{2045}', '\u{2046}'),   // LEFT SQUARE BRACKET WITH QUILL
    ('\u{2046}', '\u{2045}'),   // RIGHT SQUARE BRACKET WITH QUILL
    ('\u{207D}', '\u{207E}'),   // SUPERSCRIPT LEFT PARENTHESIS
    ('\u{207E}', '\u{207D}'),   // SUPERSCRIPT RIGHT PARENTHESIS
    ('\u{208D}', '\u{208E}'),   // SUBSCRIPT LEFT PARENTHESIS
    ('\u{208E}', '\u{208D}'),   // SUBSCRIPT RIGHT PARENTHESIS
    ('\u{2208}', '\u{220B}'),   // ELEMENT OF
    ('\u{2209}', '\u{220C}'),   // [BEST FIT] NOT AN ELEMENT OF
    ('\u{220A}', '\u{220D}'),   // SMALL ELEMENT OF
    ('\u{220B}', '\u{2208}'),   // CONTAINS AS MEMBER
    ('\u{220C}', '\u{2209}'),   // [BEST FIT] DOES NOT CONTAIN AS MEMBER
    ('\u{220D}', '\u{220A}'),   // SMALL CONTAINS AS MEMBER
    ('\u{2215}', '\u{29F5}'),   // DIVISION SLASH
    ('\u{221D}', '\u{1DB10}'),  // PROPORTIONAL TO
    ('\u{221F}', '\u{2BFE}'),   // RIGHT ANGLE
    ('\u{2220}', '\u{29A3}'),   // ANGLE
    ('\u{2221}', '\u{299B}'),   // MEASURED ANGLE
    ('\u{2222}', '\u{29A0}'),   // SPHERICAL ANGLE
    ('\u{2224}', '\u{2AEE}'),   // DOES NOT DIVIDE
    ('\u{223C}', '\u{223D}'),   // TILDE OPERATOR
    ('\u{223D}', '\u{223C}'),   // REVERSED TILDE
    ('\u{2243}', '\u{22CD}'),   // ASYMPTOTICALLY EQUAL TO
    ('\u{2245}', '\u{224C}'),   // APPROXIMATELY EQUAL TO
    ('\u{224C}', '\u{2245}'),   // ALL EQUAL TO
    ('\u{2252}', '\u{2253}'),   // APPROXIMATELY EQUAL TO OR THE IMAGE OF
    ('\u{2253}', '\u{2252}'),   // IMAGE OF OR APPROXIMATELY EQUAL TO
    ('\u{2254}', '\u{2255}'),   // COLON EQUALS
    ('\u{2255}', '\u{2254}'),   // EQUALS COLON
    ('\u{2264}', '\u{2265}'),   // LESS-THAN OR EQUAL TO
    ('\u{2265}', '\u{2264}'),   // GREATER-THAN OR EQUAL TO
    ('\u{2266}', '\u{2267}'),   // LESS-THAN OVER EQUAL TO
    ('\u{2267}', '\u{2266}'),   // GREATER-THAN OVER EQUAL TO
    ('\u{2268}', '\u{2269}'),   // [BEST FIT] LESS-THAN BUT NOT EQUAL TO
    ('\u{2269}', '\u{2268}'),   // [BEST FIT] GREATER-THAN BUT NOT EQUAL TO
    ('\u{226A}', '\u{226B}'),   // MUCH LESS-THAN
    ('\u{226B}', '\u{226A}'),   // MUCH GREATER-THAN
    ('\u{226E}', '\u{226F}'),   // [BEST FIT] NOT LESS-THAN
    ('\u{226F}', '\u{226E}'),   // [BEST FIT] NOT GREATER-THAN
    ('\u{2270}', '\u{2271}'),   // [BEST FIT] NEITHER LESS-THAN NOR EQUAL TO
    ('\u{2271}', '\u{2270}'),   // [BEST FIT] NEITHER GREATER-THAN NOR EQUAL TO
    ('\u{2272}', '\u{2273}'),   // [BEST FIT] LESS-THAN OR EQUIVALENT TO
    ('\u{2273}', '\u{2272}'),   // [BEST FIT] GREATER-THAN OR EQUIVALENT TO
    ('\u{2274}', '\u{2275}'),   // [BEST FIT] NEITHER LESS-THAN NOR EQUIVALENT TO
    ('\u{2275}', '\u{2274}'),   // [BEST FIT] NEITHER GREATER-THAN NOR EQUIVALENT TO
    ('\u{2276}', '\u{2277}'),   // LESS-THAN OR GREATER-THAN
    ('\u{2277}', '\u{2276}'),   // GREATER-THAN OR LESS-THAN
    ('\u{2278}', '\u{2279}'),   // [BEST FIT] NEITHER LESS-THAN NOR GREATER-THAN
    ('\u{2279}', '\u{2278}'),   // [BEST FIT] NEITHER GREATER-THAN NOR LESS-THAN
    ('\u{227A}', '\u{227B}'),   // PRECEDES
    ('\u{227B}', '\u{227A}'),   // SUCCEEDS
    ('\u{227C}', '\u{227D}'),   // PRECEDES OR EQUAL TO
    ('\u{227D}', '\u{227C}'),   // SUCCEEDS OR EQUAL TO
    ('\u{227E}', '\u{227F}'),   // [BEST FIT] PRECEDES OR EQUIVALENT TO
    ('\u{227F}', '\u{227E}'),   // [BEST FIT] SUCCEEDS OR EQUIVALENT TO
    ('\u{2280}', '\u{2281}'),   // [BEST FIT] DOES NOT PRECEDE
    ('\u{2281}', '\u{2280}'),   // [BEST FIT] DOES NOT SUCCEED
    ('\u{2282}', '\u{2283}'),   // SUBSET OF
    ('\u{2283}', '\u{2282}'),   // SUPERSET OF
    ('\u{2284}', '\u{2285}'),   // [BEST FIT] NOT A SUBSET OF
    ('\u{2285}', '\u{2284}'),   // [BEST FIT] NOT A SUPERSET OF
    ('\u{2286}', '\u{2287}'),   // SUBSET OF OR EQUAL TO
    ('\u{2287}', '\u{2286}'),   // SUPERSET OF OR EQUAL TO
    ('\u{2288}', '\u{2289}'),   // [BEST FIT] NEITHER A SUBSET OF NOR EQUAL TO
    ('\u{2289}', '\u{2288}'),   // [BEST FIT] NEITHER A SUPERSET OF NOR EQUAL TO
    ('\u{228A}', '\u{228B}'),   // [BEST FIT] SUBSET OF WITH NOT EQUAL TO
    ('\u{228B}', '\u{228A}'),   // [BEST FIT] SUPERSET OF WITH NOT EQUAL TO
    ('\u{228F}', '\u{2290}'),   // SQUARE IMAGE OF
    ('\u{2290}', '\u{228F}'),   // SQUARE ORIGINAL OF
    ('\u{2291}', '\u{2292}'),   // SQUARE IMAGE OF OR EQUAL TO
    ('\u{2292}', '\u{2291}'),   // SQUARE ORIGINAL OF OR EQUAL TO
    ('\u{2298}', '\u{29B8}'),   // CIRCLED DIVISION SLASH
    ('\u{22A2}', '\u{22A3}'),   // RIGHT TACK
    ('\u{22A3}', '\u{22A2}'),   // LEFT TACK
    ('\u{22A6}', '\u{2ADE}'),   // ASSERTION
    ('\u{22A8}', '\u{2AE4}'),   // TRUE
    ('\u{22A9}', '\u{2AE3}'),   // FORCES
    ('\u{22AB}', '\u{2AE5}'),   // DOUBLE VERTICAL BAR DOUBLE RIGHT TURNSTILE
    ('\u{22B0}', '\u{22B1}'),   // PRECEDES UNDER RELATION
    ('\u{22B1}', '\u{22B0}'),   // SUCCEEDS UNDER RELATION
    ('\u{22B2}', '\u{22B3}'),   // NORMAL SUBGROUP OF
    ('\u{22B3}', '\u{22B2}'),   // CONTAINS AS NORMAL SUBGROUP
    ('\u{22B4}', '\u{22B5}'),   // NORMAL SUBGROUP OF OR EQUAL TO
    ('\u{22B5}', '\u{22B4}'),   // CONTAINS AS NORMAL SUBGROUP OR EQUAL TO
    ('\u{22B6}', '\u{22B7}'),   // ORIGINAL OF
    ('\u{22B7}', '\u{22B6}'),   // IMAGE OF
    ('\u{22B8}', '\u{27DC}'),   // MULTIMAP
    ('\u{22C9}', '\u{22CA}'),   // LEFT NORMAL FACTOR SEMIDIRECT PRODUCT
    ('\u{22CA}', '\u{22C9}'),   // RIGHT NORMAL FACTOR SEMIDIRECT PRODUCT
    ('\u{22CB}', '\u{22CC}'),   // LEFT SEMIDIRECT PRODUCT
    ('\u{22CC}', '\u{22CB}'),   // RIGHT SEMIDIRECT PRODUCT
    ('\u{22CD}', '\u{2243}'),   // REVERSED TILDE EQUALS
    ('\u{22D0}', '\u{22D1}'),   // DOUBLE SUBSET
    ('\u{22D1}', '\u{22D0}'),   // DOUBLE SUPERSET
    ('\u{22D6}', '\u{22D7}'),   // LESS-THAN WITH DOT
    ('\u{22D7}', '\u{22D6}'),   // GREATER-THAN WITH DOT
    ('\u{22D8}', '\u{22D9}'),   // VERY MUCH LESS-THAN
    ('\u{22D9}', '\u{22D8}'),   // VERY MUCH GREATER-THAN
    ('\u{22DA}', '\u{22DB}'),   // LESS-THAN EQUAL TO OR GREATER-THAN
    ('\u{22DB}', '\u{22DA}'),   // GREATER-THAN EQUAL TO OR LESS-THAN
    ('\u{22DC}', '\u{22DD}'),   // EQUAL TO OR LESS-THAN
    ('\u{22DD}', '\u{22DC}'),   // EQUAL TO OR GREATER-THAN
    ('\u{22DE}', '\u{22DF}'),   // EQUAL TO OR PRECEDES
    ('\u{22DF}', '\u{22DE}'),   // EQUAL TO OR SUCCEEDS
    ('\u{22E0}', '\u{22E1}'),   // [BEST FIT] DOES NOT PRECEDE OR EQUAL
    ('\u{22E1}', '\u{22E0}'),   // [BEST FIT] DOES NOT SUCCEED OR EQUAL
    ('\u{22E2}', '\u{22E3}'),   // [BEST FIT] NOT SQUARE IMAGE OF OR EQUAL TO
    ('\u{22E3}', '\u{22E2}'),   // [BEST FIT] NOT SQUARE ORIGINAL OF OR EQUAL TO
    ('\u{22E4}', '\u{22E5}'),   // [BEST FIT] SQUARE IMAGE OF OR NOT EQUAL TO
    ('\u{22E5}', '\u{22E4}'),   // [BEST FIT] SQUARE ORIGINAL OF OR NOT EQUAL TO
    ('\u{22E6}', '\u{22E7}'),   // [BEST FIT] LESS-THAN BUT NOT EQUIVALENT TO
    ('\u{22E7}', '\u{22E6}'),   // [BEST FIT] GREATER-THAN BUT NOT EQUIVALENT TO
    ('\u{22E8}', '\u{22E9}'),   // [BEST FIT] PRECEDES BUT NOT EQUIVALENT TO
    ('\u{22E9}', '\u{22E8}'),   // [BEST FIT] SUCCEEDS BUT NOT EQUIVALENT TO
    ('\u{22EA}', '\u{22EB}'),   // [BEST FIT] NOT NORMAL SUBGROUP OF
    ('\u{22EB}', '\u{22EA}'),   // [BEST FIT] DOES NOT CONTAIN AS NORMAL SUBGROUP
    ('\u{22EC}', '\u{22ED}'),   // [BEST FIT] NOT NORMAL SUBGROUP OF OR EQUAL TO
    ('\u{22ED}', '\u{22EC}'),   // [BEST FIT] DOES NOT CONTAIN AS NORMAL SUBGROUP OR EQUAL
    ('\u{22F0}', '\u{22F1}'),   // UP RIGHT DIAGONAL ELLIPSIS
    ('\u{22F1}', '\u{22F0}'),   // DOWN RIGHT DIAGONAL ELLIPSIS
    ('\u{22F2}', '\u{22FA}'),   // ELEMENT OF WITH LONG HORIZONTAL STROKE
    ('\u{22F3}', '\u{22FB}'),   // ELEMENT OF WITH VERTICAL BAR AT END OF HORIZONTAL STROKE
    ('\u{22F4}', '\u{22FC}'),   // SMALL ELEMENT OF WITH VERTICAL BAR AT END OF HORIZONTAL STROKE
    ('\u{22F6}', '\u{22FD}'),   // ELEMENT OF WITH OVERBAR
    ('\u{22F7}', '\u{22FE}'),   // SMALL ELEMENT OF WITH OVERBAR
    ('\u{22FA}', '\u{22F2}'),   // CONTAINS WITH LONG HORIZONTAL STROKE
    ('\u{22FB}', '\u{22F3}'),   // CONTAINS WITH VERTICAL BAR AT END OF HORIZONTAL STROKE
    ('\u{22FC}', '\u{22F4}'),   // SMALL CONTAINS WITH VERTICAL BAR AT END OF HORIZONTAL STROKE
    ('\u{22FD}', '\u{22F6}'),   // CONTAINS WITH OVERBAR
    ('\u{22FE}', '\u{22F7}'),   // SMALL CONTAINS WITH OVERBAR
    ('\u{2308}', '\u{2309}'),   // LEFT CEILING
    ('\u{2309}', '\u{2308}'),   // RIGHT CEILING
    ('\u{230A}', '\u{230B}'),   // LEFT FLOOR
    ('\u{230B}', '\u{230A}'),   // RIGHT FLOOR
    ('\u{2329}', '\u{232A}'),   // LEFT-POINTING ANGLE BRACKET
    ('\u{232A}', '\u{2329}'),   // RIGHT-POINTING ANGLE BRACKET
    ('\u{2768}', '\u{2769}'),   // MEDIUM LEFT PARENTHESIS ORNAMENT
    ('\u{2769}', '\u{2768}'),   // MEDIUM RIGHT PARENTHESIS ORNAMENT
    ('\u{276A}', '\u{276B}'),   // MEDIUM FLATTENED LEFT PARENTHESIS ORNAMENT
    ('\u{276B}', '\u{276A}'),   // MEDIUM FLATTENED RIGHT PARENTHESIS ORNAMENT
    ('\u{276C}', '\u{276D}'),   // MEDIUM LEFT-POINTING ANGLE BRACKET ORNAMENT
    ('\u{276D}', '\u{276C}'),   // MEDIUM RIGHT-POINTING ANGLE BRACKET ORNAMENT
    ('\u{276E}', '\u{276F}'),   // HEAVY LEFT-POINTING ANGLE QUOTATION MARK ORNAMENT
    ('\u{276F}', '\u{276E}'),   // HEAVY RIGHT-POINTING ANGLE QUOTATION MARK ORNAMENT
    ('\u{2770}', '\u{2771}'),   // HEAVY LEFT-POINTING ANGLE BRACKET ORNAMENT
    ('\u{2771}', '\u{2770}'),   // HEAVY RIGHT-POINTING ANGLE BRACKET ORNAMENT
    ('\u{2772}', '\u{2773}'),   // LIGHT LEFT TORTOISE SHELL BRACKET ORNAMENT
    ('\u{2773}', '\u{2772}'),   // LIGHT RIGHT TORTOISE SHELL BRACKET ORNAMENT
    ('\u{2774}', '\u{2775}'),   // MEDIUM LEFT CURLY BRACKET ORNAMENT
    ('\u{2775}', '\u{2774}'),   // MEDIUM RIGHT CURLY BRACKET ORNAMENT
    ('\u{27C3}', '\u{27C4}'),   // OPEN SUBSET
    ('\u{27C4}', '\u{27C3}'),   // OPEN SUPERSET
    ('\u{27C5}', '\u{27C6}'),   // LEFT S-SHAPED BAG DELIMITER
    ('\u{27C6}', '\u{27C5}'),   // RIGHT S-SHAPED BAG DELIMITER
    ('\u{27C8}', '\u{27C9}'),   // REVERSE SOLIDUS PRECEDING SUBSET
    ('\u{27C9}', '\u{27C8}'),   // SUPERSET PRECEDING SOLIDUS
    ('\u{27CB}', '\u{27CD}'),   // MATHEMATICAL RISING DIAGONAL
    ('\u{27CD}', '\u{27CB}'),   // MATHEMATICAL FALLING DIAGONAL
    ('\u{27D5}', '\u{27D6}'),   // LEFT OUTER JOIN
    ('\u{27D6}', '\u{27D5}'),   // RIGHT OUTER JOIN
    ('\u{27DC}', '\u{22B8}'),   // LEFT MULTIMAP
    ('\u{27DD}', '\u{27DE}'),   // LONG RIGHT TACK
    ('\u{27DE}', '\u{27DD}'),   // LONG LEFT TACK
    ('\u{27E2}', '\u{27E3}'),   // WHITE CONCAVE-SIDED DIAMOND WITH LEFTWARDS TICK
    ('\u{27E3}', '\u{27E2}'),   // WHITE CONCAVE-SIDED DIAMOND WITH RIGHTWARDS TICK
    ('\u{27E4}', '\u{27E5}'),   // WHITE SQUARE WITH LEFTWARDS TICK
    ('\u{27E5}', '\u{27E4}'),   // WHITE SQUARE WITH RIGHTWARDS TICK
    ('\u{27E6}', '\u{27E7}'),   // MATHEMATICAL LEFT WHITE SQUARE BRACKET
    ('\u{27E7}', '\u{27E6}'),   // MATHEMATICAL RIGHT WHITE SQUARE BRACKET
    ('\u{27E8}', '\u{27E9}'),   // MATHEMATICAL LEFT ANGLE BRACKET
    ('\u{27E9}', '\u{27E8}'),   // MATHEMATICAL RIGHT ANGLE BRACKET
    ('\u{27EA}', '\u{27EB}'),   // MATHEMATICAL LEFT DOUBLE ANGLE BRACKET
    ('\u{27EB}', '\u{27EA}'),   // MATHEMATICAL RIGHT DOUBLE ANGLE BRACKET
    ('\u{27EC}', '\u{27ED}'),   // MATHEMATICAL LEFT WHITE TORTOISE SHELL BRACKET
    ('\u{27ED}', '\u{27EC}'),   // MATHEMATICAL RIGHT WHITE TORTOISE SHELL BRACKET
    ('\u{27EE}', '\u{27EF}'),   // MATHEMATICAL LEFT FLATTENED PARENTHESIS
    ('\u{27EF}', '\u{27EE}'),   // MATHEMATICAL RIGHT FLATTENED PARENTHESIS
    ('\u{2983}', '\u{2984}'),   // LEFT WHITE CURLY BRACKET
    ('\u{2984}', '\u{2983}'),   // RIGHT WHITE CURLY BRACKET
    ('\u{2985}', '\u{2986}'),   // LEFT WHITE PARENTHESIS
    ('\u{2986}', '\u{2985}'),   // RIGHT WHITE PARENTHESIS
    ('\u{2987}', '\u{2988}'),   // Z NOTATION LEFT IMAGE BRACKET
    ('\u{2988}', '\u{2987}'),   // Z NOTATION RIGHT IMAGE BRACKET
    ('\u{2989}', '\u{298A}'),   // Z NOTATION LEFT BINDING BRACKET
    ('\u{298A}', '\u{2989}'),   // Z NOTATION RIGHT BINDING BRACKET
    ('\u{298B}', '\u{298C}'),   // LEFT SQUARE BRACKET WITH UNDERBAR
    ('\u{298C}', '\u{298B}'),   // RIGHT SQUARE BRACKET WITH UNDERBAR
    ('\u{298D}', '\u{2990}'),   // LEFT SQUARE BRACKET WITH TICK IN TOP CORNER
    ('\u{298E}', '\u{298F}'),   // RIGHT SQUARE BRACKET WITH TICK IN BOTTOM CORNER
    ('\u{298F}', '\u{298E}'),   // LEFT SQUARE BRACKET WITH TICK IN BOTTOM CORNER
    ('\u{2990}', '\u{298D}'),   // RIGHT SQUARE BRACKET WITH TICK IN TOP CORNER
    ('\u{2991}', '\u{2992}'),   // LEFT ANGLE BRACKET WITH DOT
    ('\u{2992}', '\u{2991}'),   // RIGHT ANGLE BRACKET WITH DOT
    ('\u{2993}', '\u{2994}'),   // LEFT ARC LESS-THAN BRACKET
    ('\u{2994}', '\u{2993}'),   // RIGHT ARC GREATER-THAN BRACKET
    ('\u{2995}', '\u{2996}'),   // DOUBLE LEFT ARC GREATER-THAN BRACKET
    ('\u{2996}', '\u{2995}'),   // DOUBLE RIGHT ARC LESS-THAN BRACKET
    ('\u{2997}', '\u{2998}'),   // LEFT BLACK TORTOISE SHELL BRACKET
    ('\u{2998}', '\u{2997}'),   // RIGHT BLACK TORTOISE SHELL BRACKET
    ('\u{299B}', '\u{2221}'),   // MEASURED ANGLE OPENING LEFT
    ('\u{29A0}', '\u{2222}'),   // SPHERICAL ANGLE OPENING LEFT
    ('\u{29A3}', '\u{2220}'),   // REVERSED ANGLE
    ('\u{29A4}', '\u{29A5}'),   // ANGLE WITH UNDERBAR
    ('\u{29A5}', '\u{29A4}'),   // REVERSED ANGLE WITH UNDERBAR
    ('\u{29A8}', '\u{29A9}'), // MEASURED ANGLE WITH OPEN ARM ENDING IN ARROW POINTING UP AND RIGHT
    ('\u{29A9}', '\u{29A8}'), // MEASURED ANGLE WITH OPEN ARM ENDING IN ARROW POINTING UP AND LEFT
    ('\u{29AA}', '\u{29AB}'), // MEASURED ANGLE WITH OPEN ARM ENDING IN ARROW POINTING DOWN AND RIGHT
    ('\u{29AB}', '\u{29AA}'), // MEASURED ANGLE WITH OPEN ARM ENDING IN ARROW POINTING DOWN AND LEFT
    ('\u{29AC}', '\u{29AD}'), // MEASURED ANGLE WITH OPEN ARM ENDING IN ARROW POINTING RIGHT AND UP
    ('\u{29AD}', '\u{29AC}'), // MEASURED ANGLE WITH OPEN ARM ENDING IN ARROW POINTING LEFT AND UP
    ('\u{29AE}', '\u{29AF}'), // MEASURED ANGLE WITH OPEN ARM ENDING IN ARROW POINTING RIGHT AND DOWN
    ('\u{29AF}', '\u{29AE}'), // MEASURED ANGLE WITH OPEN ARM ENDING IN ARROW POINTING LEFT AND DOWN
    ('\u{29B8}', '\u{2298}'), // CIRCLED REVERSE SOLIDUS
    ('\u{29C0}', '\u{29C1}'), // CIRCLED LESS-THAN
    ('\u{29C1}', '\u{29C0}'), // CIRCLED GREATER-THAN
    ('\u{29C4}', '\u{29C5}'), // SQUARED RISING DIAGONAL SLASH
    ('\u{29C5}', '\u{29C4}'), // SQUARED FALLING DIAGONAL SLASH
    ('\u{29CF}', '\u{29D0}'), // LEFT TRIANGLE BESIDE VERTICAL BAR
    ('\u{29D0}', '\u{29CF}'), // VERTICAL BAR BESIDE RIGHT TRIANGLE
    ('\u{29D1}', '\u{29D2}'), // BOWTIE WITH LEFT HALF BLACK
    ('\u{29D2}', '\u{29D1}'), // BOWTIE WITH RIGHT HALF BLACK
    ('\u{29D4}', '\u{29D5}'), // TIMES WITH LEFT HALF BLACK
    ('\u{29D5}', '\u{29D4}'), // TIMES WITH RIGHT HALF BLACK
    ('\u{29D8}', '\u{29D9}'), // LEFT WIGGLY FENCE
    ('\u{29D9}', '\u{29D8}'), // RIGHT WIGGLY FENCE
    ('\u{29DA}', '\u{29DB}'), // LEFT DOUBLE WIGGLY FENCE
    ('\u{29DB}', '\u{29DA}'), // RIGHT DOUBLE WIGGLY FENCE
    ('\u{29E8}', '\u{29E9}'), // DOWN-POINTING TRIANGLE WITH LEFT HALF BLACK
    ('\u{29E9}', '\u{29E8}'), // DOWN-POINTING TRIANGLE WITH RIGHT HALF BLACK
    ('\u{29F5}', '\u{2215}'), // REVERSE SOLIDUS OPERATOR
    ('\u{29F8}', '\u{29F9}'), // BIG SOLIDUS
    ('\u{29F9}', '\u{29F8}'), // BIG REVERSE SOLIDUS
    ('\u{29FC}', '\u{29FD}'), // LEFT-POINTING CURVED ANGLE BRACKET
    ('\u{29FD}', '\u{29FC}'), // RIGHT-POINTING CURVED ANGLE BRACKET
    ('\u{2A2B}', '\u{2A2C}'), // MINUS SIGN WITH FALLING DOTS
    ('\u{2A2C}', '\u{2A2B}'), // MINUS SIGN WITH RISING DOTS
    ('\u{2A2D}', '\u{2A2E}'), // PLUS SIGN IN LEFT HALF CIRCLE
    ('\u{2A2E}', '\u{2A2D}'), // PLUS SIGN IN RIGHT HALF CIRCLE
    ('\u{2A34}', '\u{2A35}'), // MULTIPLICATION SIGN IN LEFT HALF CIRCLE
    ('\u{2A35}', '\u{2A34}'), // MULTIPLICATION SIGN IN RIGHT HALF CIRCLE
    ('\u{2A3C}', '\u{2A3D}'), // INTERIOR PRODUCT
    ('\u{2A3D}', '\u{2A3C}'), // RIGHTHAND INTERIOR PRODUCT
    ('\u{2A64}', '\u{2A65}'), // Z NOTATION DOMAIN ANTIRESTRICTION
    ('\u{2A65}', '\u{2A64}'), // Z NOTATION RANGE ANTIRESTRICTION
    ('\u{2A79}', '\u{2A7A}'), // LESS-THAN WITH CIRCLE INSIDE
    ('\u{2A7A}', '\u{2A79}'), // GREATER-THAN WITH CIRCLE INSIDE
    ('\u{2A7B}', '\u{2A7C}'), // [BEST FIT] LESS-THAN WITH QUESTION MARK ABOVE
    ('\u{2A7C}', '\u{2A7B}'), // [BEST FIT] GREATER-THAN WITH QUESTION MARK ABOVE
    ('\u{2A7D}', '\u{2A7E}'), // LESS-THAN OR SLANTED EQUAL TO
    ('\u{2A7E}', '\u{2A7D}'), // GREATER-THAN OR SLANTED EQUAL TO
    ('\u{2A7F}', '\u{2A80}'), // LESS-THAN OR SLANTED EQUAL TO WITH DOT INSIDE
    ('\u{2A80}', '\u{2A7F}'), // GREATER-THAN OR SLANTED EQUAL TO WITH DOT INSIDE
    ('\u{2A81}', '\u{2A82}'), // LESS-THAN OR SLANTED EQUAL TO WITH DOT ABOVE
    ('\u{2A82}', '\u{2A81}'), // GREATER-THAN OR SLANTED EQUAL TO WITH DOT ABOVE
    ('\u{2A83}', '\u{2A84}'), // LESS-THAN OR SLANTED EQUAL TO WITH DOT ABOVE RIGHT
    ('\u{2A84}', '\u{2A83}'), // GREATER-THAN OR SLANTED EQUAL TO WITH DOT ABOVE LEFT
    ('\u{2A85}', '\u{2A86}'), // [BEST FIT] LESS-THAN OR APPROXIMATE
    ('\u{2A86}', '\u{2A85}'), // [BEST FIT] GREATER-THAN OR APPROXIMATE
    ('\u{2A87}', '\u{2A88}'), // [BEST FIT] LESS-THAN AND SINGLE-LINE NOT EQUAL TO
    ('\u{2A88}', '\u{2A87}'), // [BEST FIT] GREATER-THAN AND SINGLE-LINE NOT EQUAL TO
    ('\u{2A89}', '\u{2A8A}'), // [BEST FIT] LESS-THAN AND NOT APPROXIMATE
    ('\u{2A8A}', '\u{2A89}'), // [BEST FIT] GREATER-THAN AND NOT APPROXIMATE
    ('\u{2A8B}', '\u{2A8C}'), // LESS-THAN ABOVE DOUBLE-LINE EQUAL ABOVE GREATER-THAN
    ('\u{2A8C}', '\u{2A8B}'), // GREATER-THAN ABOVE DOUBLE-LINE EQUAL ABOVE LESS-THAN
    ('\u{2A8D}', '\u{2A8E}'), // [BEST FIT] LESS-THAN ABOVE SIMILAR OR EQUAL
    ('\u{2A8E}', '\u{2A8D}'), // [BEST FIT] GREATER-THAN ABOVE SIMILAR OR EQUAL
    ('\u{2A8F}', '\u{2A90}'), // [BEST FIT] LESS-THAN ABOVE SIMILAR ABOVE GREATER-THAN
    ('\u{2A90}', '\u{2A8F}'), // [BEST FIT] GREATER-THAN ABOVE SIMILAR ABOVE LESS-THAN
    ('\u{2A91}', '\u{2A92}'), // LESS-THAN ABOVE GREATER-THAN ABOVE DOUBLE-LINE EQUAL
    ('\u{2A92}', '\u{2A91}'), // GREATER-THAN ABOVE LESS-THAN ABOVE DOUBLE-LINE EQUAL
    ('\u{2A93}', '\u{2A94}'), // LESS-THAN ABOVE SLANTED EQUAL ABOVE GREATER-THAN ABOVE SLANTED EQUAL
    ('\u{2A94}', '\u{2A93}'), // GREATER-THAN ABOVE SLANTED EQUAL ABOVE LESS-THAN ABOVE SLANTED EQUAL
    ('\u{2A95}', '\u{2A96}'), // SLANTED EQUAL TO OR LESS-THAN
    ('\u{2A96}', '\u{2A95}'), // SLANTED EQUAL TO OR GREATER-THAN
    ('\u{2A97}', '\u{2A98}'), // SLANTED EQUAL TO OR LESS-THAN WITH DOT INSIDE
    ('\u{2A98}', '\u{2A97}'), // SLANTED EQUAL TO OR GREATER-THAN WITH DOT INSIDE
    ('\u{2A99}', '\u{2A9A}'), // DOUBLE-LINE EQUAL TO OR LESS-THAN
    ('\u{2A9A}', '\u{2A99}'), // DOUBLE-LINE EQUAL TO OR GREATER-THAN
    ('\u{2A9B}', '\u{2A9C}'), // DOUBLE-LINE SLANTED EQUAL TO OR LESS-THAN
    ('\u{2A9C}', '\u{2A9B}'), // DOUBLE-LINE SLANTED EQUAL TO OR GREATER-THAN
    ('\u{2A9D}', '\u{2A9E}'), // [BEST FIT] SIMILAR OR LESS-THAN
    ('\u{2A9E}', '\u{2A9D}'), // [BEST FIT] SIMILAR OR GREATER-THAN
    ('\u{2A9F}', '\u{2AA0}'), // [BEST FIT] SIMILAR ABOVE LESS-THAN ABOVE EQUALS SIGN
    ('\u{2AA0}', '\u{2A9F}'), // [BEST FIT] SIMILAR ABOVE GREATER-THAN ABOVE EQUALS SIGN
    ('\u{2AA1}', '\u{2AA2}'), // DOUBLE NESTED LESS-THAN
    ('\u{2AA2}', '\u{2AA1}'), // DOUBLE NESTED GREATER-THAN
    ('\u{2AA6}', '\u{2AA7}'), // LESS-THAN CLOSED BY CURVE
    ('\u{2AA7}', '\u{2AA6}'), // GREATER-THAN CLOSED BY CURVE
    ('\u{2AA8}', '\u{2AA9}'), // LESS-THAN CLOSED BY CURVE ABOVE SLANTED EQUAL
    ('\u{2AA9}', '\u{2AA8}'), // GREATER-THAN CLOSED BY CURVE ABOVE SLANTED EQUAL
    ('\u{2AAA}', '\u{2AAB}'), // SMALLER THAN
    ('\u{2AAB}', '\u{2AAA}'), // LARGER THAN
    ('\u{2AAC}', '\u{2AAD}'), // SMALLER THAN OR EQUAL TO
    ('\u{2AAD}', '\u{2AAC}'), // LARGER THAN OR EQUAL TO
    ('\u{2AAF}', '\u{2AB0}'), // PRECEDES ABOVE SINGLE-LINE EQUALS SIGN
    ('\u{2AB0}', '\u{2AAF}'), // SUCCEEDS ABOVE SINGLE-LINE EQUALS SIGN
    ('\u{2AB1}', '\u{2AB2}'), // [BEST FIT] PRECEDES ABOVE SINGLE-LINE NOT EQUAL TO
    ('\u{2AB2}', '\u{2AB1}'), // [BEST FIT] SUCCEEDS ABOVE SINGLE-LINE NOT EQUAL TO
    ('\u{2AB3}', '\u{2AB4}'), // PRECEDES ABOVE EQUALS SIGN
    ('\u{2AB4}', '\u{2AB3}'), // SUCCEEDS ABOVE EQUALS SIGN
    ('\u{2AB5}', '\u{2AB6}'), // [BEST FIT] PRECEDES ABOVE NOT EQUAL TO
    ('\u{2AB6}', '\u{2AB5}'), // [BEST FIT] SUCCEEDS ABOVE NOT EQUAL TO
    ('\u{2AB7}', '\u{2AB8}'), // [BEST FIT] PRECEDES ABOVE ALMOST EQUAL TO
    ('\u{2AB8}', '\u{2AB7}'), // [BEST FIT] SUCCEEDS ABOVE ALMOST EQUAL TO
    ('\u{2AB9}', '\u{2ABA}'), // [BEST FIT] PRECEDES ABOVE NOT ALMOST EQUAL TO
    ('\u{2ABA}', '\u{2AB9}'), // [BEST FIT] SUCCEEDS ABOVE NOT ALMOST EQUAL TO
    ('\u{2ABB}', '\u{2ABC}'), // DOUBLE PRECEDES
    ('\u{2ABC}', '\u{2ABB}'), // DOUBLE SUCCEEDS
    ('\u{2ABD}', '\u{2ABE}'), // SUBSET WITH DOT
    ('\u{2ABE}', '\u{2ABD}'), // SUPERSET WITH DOT
    ('\u{2ABF}', '\u{2AC0}'), // SUBSET WITH PLUS SIGN BELOW
    ('\u{2AC0}', '\u{2ABF}'), // SUPERSET WITH PLUS SIGN BELOW
    ('\u{2AC1}', '\u{2AC2}'), // SUBSET WITH MULTIPLICATION SIGN BELOW
    ('\u{2AC2}', '\u{2AC1}'), // SUPERSET WITH MULTIPLICATION SIGN BELOW
    ('\u{2AC3}', '\u{2AC4}'), // SUBSET OF OR EQUAL TO WITH DOT ABOVE
    ('\u{2AC4}', '\u{2AC3}'), // SUPERSET OF OR EQUAL TO WITH DOT ABOVE
    ('\u{2AC5}', '\u{2AC6}'), // SUBSET OF ABOVE EQUALS SIGN
    ('\u{2AC6}', '\u{2AC5}'), // SUPERSET OF ABOVE EQUALS SIGN
    ('\u{2AC7}', '\u{2AC8}'), // [BEST FIT] SUBSET OF ABOVE TILDE OPERATOR
    ('\u{2AC8}', '\u{2AC7}'), // [BEST FIT] SUPERSET OF ABOVE TILDE OPERATOR
    ('\u{2AC9}', '\u{2ACA}'), // [BEST FIT] SUBSET OF ABOVE ALMOST EQUAL TO
    ('\u{2ACA}', '\u{2AC9}'), // [BEST FIT] SUPERSET OF ABOVE ALMOST EQUAL TO
    ('\u{2ACB}', '\u{2ACC}'), // [BEST FIT] SUBSET OF ABOVE NOT EQUAL TO
    ('\u{2ACC}', '\u{2ACB}'), // [BEST FIT] SUPERSET OF ABOVE NOT EQUAL TO
    ('\u{2ACD}', '\u{2ACE}'), // SQUARE LEFT OPEN BOX OPERATOR
    ('\u{2ACE}', '\u{2ACD}'), // SQUARE RIGHT OPEN BOX OPERATOR
    ('\u{2ACF}', '\u{2AD0}'), // CLOSED SUBSET
    ('\u{2AD0}', '\u{2ACF}'), // CLOSED SUPERSET
    ('\u{2AD1}', '\u{2AD2}'), // CLOSED SUBSET OR EQUAL TO
    ('\u{2AD2}', '\u{2AD1}'), // CLOSED SUPERSET OR EQUAL TO
    ('\u{2AD3}', '\u{2AD4}'), // SUBSET ABOVE SUPERSET
    ('\u{2AD4}', '\u{2AD3}'), // SUPERSET ABOVE SUBSET
    ('\u{2AD5}', '\u{2AD6}'), // SUBSET ABOVE SUBSET
    ('\u{2AD6}', '\u{2AD5}'), // SUPERSET ABOVE SUPERSET
    ('\u{2ADE}', '\u{22A6}'), // SHORT LEFT TACK
    ('\u{2AE3}', '\u{22A9}'), // DOUBLE VERTICAL BAR LEFT TURNSTILE
    ('\u{2AE4}', '\u{22A8}'), // VERTICAL BAR DOUBLE LEFT TURNSTILE
    ('\u{2AE5}', '\u{22AB}'), // DOUBLE VERTICAL BAR DOUBLE LEFT TURNSTILE
    ('\u{2AEC}', '\u{2AED}'), // DOUBLE STROKE NOT SIGN
    ('\u{2AED}', '\u{2AEC}'), // REVERSED DOUBLE STROKE NOT SIGN
    ('\u{2AEE}', '\u{2224}'), // DOES NOT DIVIDE WITH REVERSED NEGATION SLASH
    ('\u{2AF7}', '\u{2AF8}'), // TRIPLE NESTED LESS-THAN
    ('\u{2AF8}', '\u{2AF7}'), // TRIPLE NESTED GREATER-THAN
    ('\u{2AF9}', '\u{2AFA}'), // DOUBLE-LINE SLANTED LESS-THAN OR EQUAL TO
    ('\u{2AFA}', '\u{2AF9}'), // DOUBLE-LINE SLANTED GREATER-THAN OR EQUAL TO
    ('\u{2BFE}', '\u{221F}'), // REVERSED RIGHT ANGLE
    ('\u{2E02}', '\u{2E03}'), // LEFT SUBSTITUTION BRACKET
    ('\u{2E03}', '\u{2E02}'), // RIGHT SUBSTITUTION BRACKET
    ('\u{2E04}', '\u{2E05}'), // LEFT DOTTED SUBSTITUTION BRACKET
    ('\u{2E05}', '\u{2E04}'), // RIGHT DOTTED SUBSTITUTION BRACKET
    ('\u{2E09}', '\u{2E0A}'), // LEFT TRANSPOSITION BRACKET
    ('\u{2E0A}', '\u{2E09}'), // RIGHT TRANSPOSITION BRACKET
    ('\u{2E0C}', '\u{2E0D}'), // LEFT RAISED OMISSION BRACKET
    ('\u{2E0D}', '\u{2E0C}'), // RIGHT RAISED OMISSION BRACKET
    ('\u{2E1C}', '\u{2E1D}'), // LEFT LOW PARAPHRASE BRACKET
    ('\u{2E1D}', '\u{2E1C}'), // RIGHT LOW PARAPHRASE BRACKET
    ('\u{2E20}', '\u{2E21}'), // LEFT VERTICAL BAR WITH QUILL
    ('\u{2E21}', '\u{2E20}'), // RIGHT VERTICAL BAR WITH QUILL
    ('\u{2E22}', '\u{2E23}'), // TOP LEFT HALF BRACKET
    ('\u{2E23}', '\u{2E22}'), // TOP RIGHT HALF BRACKET
    ('\u{2E24}', '\u{2E25}'), // BOTTOM LEFT HALF BRACKET
    ('\u{2E25}', '\u{2E24}'), // BOTTOM RIGHT HALF BRACKET
    ('\u{2E26}', '\u{2E27}'), // LEFT SIDEWAYS U BRACKET
    ('\u{2E27}', '\u{2E26}'), // RIGHT SIDEWAYS U BRACKET
    ('\u{2E28}', '\u{2E29}'), // LEFT DOUBLE PARENTHESIS
    ('\u{2E29}', '\u{2E28}'), // RIGHT DOUBLE PARENTHESIS
    ('\u{2E55}', '\u{2E56}'), // LEFT SQUARE BRACKET WITH STROKE
    ('\u{2E56}', '\u{2E55}'), // RIGHT SQUARE BRACKET WITH STROKE
    ('\u{2E57}', '\u{2E58}'), // LEFT SQUARE BRACKET WITH DOUBLE STROKE
    ('\u{2E58}', '\u{2E57}'), // RIGHT SQUARE BRACKET WITH DOUBLE STROKE
    ('\u{2E59}', '\u{2E5A}'), // TOP HALF LEFT PARENTHESIS
    ('\u{2E5A}', '\u{2E59}'), // TOP HALF RIGHT PARENTHESIS
    ('\u{2E5B}', '\u{2E5C}'), // BOTTOM HALF LEFT PARENTHESIS
    ('\u{2E5C}', '\u{2E5B}'), // BOTTOM HALF RIGHT PARENTHESIS
    ('\u{2E62}', '\u{2E63}'), // LEFT PARENTHESIS WITH MIDDLE RING
    ('\u{2E63}', '\u{2E62}'), // RIGHT PARENTHESIS WITH MIDDLE RING
    ('\u{3008}', '\u{3009}'), // LEFT ANGLE BRACKET
    ('\u{3009}', '\u{3008}'), // RIGHT ANGLE BRACKET
    ('\u{300A}', '\u{300B}'), // LEFT DOUBLE ANGLE BRACKET
    ('\u{300B}', '\u{300A}'), // RIGHT DOUBLE ANGLE BRACKET
    ('\u{300C}', '\u{300D}'), // [BEST FIT] LEFT CORNER BRACKET
    ('\u{300D}', '\u{300C}'), // [BEST FIT] RIGHT CORNER BRACKET
    ('\u{300E}', '\u{300F}'), // [BEST FIT] LEFT WHITE CORNER BRACKET
    ('\u{300F}', '\u{300E}'), // [BEST FIT] RIGHT WHITE CORNER BRACKET
    ('\u{3010}', '\u{3011}'), // LEFT BLACK LENTICULAR BRACKET
    ('\u{3011}', '\u{3010}'), // RIGHT BLACK LENTICULAR BRACKET
    ('\u{3014}', '\u{3015}'), // LEFT TORTOISE SHELL BRACKET
    ('\u{3015}', '\u{3014}'), // RIGHT TORTOISE SHELL BRACKET
    ('\u{3016}', '\u{3017}'), // LEFT WHITE LENTICULAR BRACKET
    ('\u{3017}', '\u{3016}'), // RIGHT WHITE LENTICULAR BRACKET
    ('\u{3018}', '\u{3019}'), // LEFT WHITE TORTOISE SHELL BRACKET
    ('\u{3019}', '\u{3018}'), // RIGHT WHITE TORTOISE SHELL BRACKET
    ('\u{301A}', '\u{301B}'), // LEFT WHITE SQUARE BRACKET
    ('\u{301B}', '\u{301A}'), // RIGHT WHITE SQUARE BRACKET
    ('\u{FE59}', '\u{FE5A}'), // SMALL LEFT PARENTHESIS
    ('\u{FE5A}', '\u{FE59}'), // SMALL RIGHT PARENTHESIS
    ('\u{FE5B}', '\u{FE5C}'), // SMALL LEFT CURLY BRACKET
    ('\u{FE5C}', '\u{FE5B}'), // SMALL RIGHT CURLY BRACKET
    ('\u{FE5D}', '\u{FE5E}'), // SMALL LEFT TORTOISE SHELL BRACKET
    ('\u{FE5E}', '\u{FE5D}'), // SMALL RIGHT TORTOISE SHELL BRACKET
    ('\u{FE64}', '\u{FE65}'), // SMALL LESS-THAN SIGN
    ('\u{FE65}', '\u{FE64}'), // SMALL GREATER-THAN SIGN
    ('\u{FF08}', '\u{FF09}'), // FULLWIDTH LEFT PARENTHESIS
    ('\u{FF09}', '\u{FF08}'), // FULLWIDTH RIGHT PARENTHESIS
    ('\u{FF1C}', '\u{FF1E}'), // FULLWIDTH LESS-THAN SIGN
    ('\u{FF1E}', '\u{FF1C}'), // FULLWIDTH GREATER-THAN SIGN
    ('\u{FF3B}', '\u{FF3D}'), // FULLWIDTH LEFT SQUARE BRACKET
    ('\u{FF3D}', '\u{FF3B}'), // FULLWIDTH RIGHT SQUARE BRACKET
    ('\u{FF5B}', '\u{FF5D}'), // FULLWIDTH LEFT CURLY BRACKET
    ('\u{FF5D}', '\u{FF5B}'), // FULLWIDTH RIGHT CURLY BRACKET
    ('\u{FF5F}', '\u{FF60}'), // FULLWIDTH LEFT WHITE PARENTHESIS
    ('\u{FF60}', '\u{FF5F}'), // FULLWIDTH RIGHT WHITE PARENTHESIS
    ('\u{FF62}', '\u{FF63}'), // [BEST FIT] HALFWIDTH LEFT CORNER BRACKET
    ('\u{FF63}', '\u{FF62}'), // [BEST FIT] HALFWIDTH RIGHT CORNER BRACKET
    ('\u{1DB03}', '\u{1DB04}'), // LEIBNIZIAN GREATER-THAN
    ('\u{1DB04}', '\u{1DB03}'), // LEIBNIZIAN LESS-THAN
    ('\u{1DB05}', '\u{1DB06}'), // LEIBNIZIAN GREATER-THAN WITH SMALL P
    ('\u{1DB06}', '\u{1DB05}'), // LEIBNIZIAN LESS-THAN WITH SMALL P
    ('\u{1DB08}', '\u{1DB09}'), // INVERTED SQUARE LEFT OPEN BOX OPERATOR
    ('\u{1DB09}', '\u{1DB08}'), // INVERTED SQUARE RIGHT OPEN BOX OPERATOR
    ('\u{1DB10}', '\u{221D}'), // CARTESIAN EQUALS SIGN
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_symmetric() {
        assert!(MIRRORING.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for &(source, image) in MIRRORING {
            assert_eq!(mirrored(image), Some(source), "{source:?}");
        }
    }

    #[test]
    fn brackets_and_relations_mirror_letters_do_not() {
        assert_eq!(mirrored('('), Some(')'));
        assert_eq!(mirrored(')'), Some('('));
        assert_eq!(mirrored('\u{2265}'), Some('\u{2264}'));
        assert_eq!(mirrored('\u{29F5}'), Some('\u{2215}'));
        assert_eq!(mirrored('a'), None);
        assert_eq!(mirrored('\u{05D0}'), None);
    }
}
