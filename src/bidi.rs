//! Logical order for right-to-left text painted in visual order.
//!
//! A PDF content stream positions glyphs, not characters. A producer that has
//! already laid a line out with the Unicode Bidirectional Algorithm (UBA)
//! paints the glyphs in display order, so the decoded text of a
//! right-to-left line comes back in *visual* order: read left to right, every
//! Hebrew or Arabic word is spelled backwards, embedded numbers and Latin
//! words read forwards, and a bracket is the mirrored glyph that was drawn
//! rather than the character that was written. This module inverts that
//! layout: given a line's characters in screen order it recovers the logical
//! (reading) order the UBA started from.
//!
//! The inversion works on resolved embedding levels. Rule L2 of the UBA
//! turns a logical line into a display line by reversing, from the highest
//! level down to the lowest odd level, every run of characters at that level
//! or higher; applying the same reversals from the lowest level upwards
//! undoes it. The levels themselves are a property of the characters, so
//! they are computed (with the `unicode_bidi` implementation of the UBA) on
//! the display line read back in the paragraph direction, where all but the
//! embedded runs are already in logical order. Paired brackets are matched
//! on the display line, where they nest properly whichever way they were
//! written, and take their level from the rule N0 conditions evaluated there;
//! brackets that end up at an odd level are the mirrored glyphs of the
//! characters that were written and are mirrored back (rule L4 in reverse).
//!
//! The same machinery renders logical text in display order, which is how a
//! line whose items already hold logical text (a word per show operator,
//! the convention of OCR text layers) is put through the same reordering as
//! a line of visual-order items.

use std::borrow::Cow;

use unicode_bidi::{bidi_class, BidiClass, BidiDataSource, BidiInfo, HardcodedBidiData, Level};
use unicode_normalization::UnicodeNormalization;

/// One character of a line in screen order, tagged with the item it belongs
/// to. `None` marks a word gap synthesized between two items: it takes part
/// in the resolution of neutral characters and is dropped afterwards.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VisualChar {
    pub ch: char,
    pub item: Option<usize>,
}

/// The mirror image of `c` (`Bidi_Mirroring_Glyph`), or `c` itself when
/// it has none. Rule L4 displays a mirrored character at an odd level by
/// its mirror image; a bracket painted in a right-to-left word is turned
/// back into the character that was written with the same data.
pub(crate) fn mirror_char(c: char) -> char {
    crate::bidi_mirroring::mirrored(c).unwrap_or(c)
}

/// For a paired bracket (`Bidi_Paired_Bracket_Type`), its pair's opening
/// glyph — canonical equivalents such as the two angle brackets share one
/// — and whether `c` is that opening glyph.
fn paired_bracket(c: char) -> Option<(char, bool)> {
    HardcodedBidiData
        .bidi_matched_opening_bracket(c)
        .map(|pair| (pair.opening, pair.is_open))
}

/// Combining marks by general category (Mn) or nonzero canonical combining
/// class. Both signals are needed: Thaana vowel signs are Mn with ccc 0,
/// while some reordering marks are not Mn.
pub(crate) fn is_combining_mark(c: char) -> bool {
    unicode_normalization::char::is_combining_mark(c)
        || unicode_normalization::char::canonical_combining_class(c) != 0
}

/// The direction a character contributes to rule N0: `L`, or `R` for the
/// right-to-left classes and for numbers, which N0 treats as `R`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strong {
    Left,
    Right,
}

/// The direction of a letter: `L`, or `R` for the right-to-left classes.
/// Digits are not letters here.
fn letter_direction(c: char) -> Option<Strong> {
    match class_of(c) {
        BidiClass::L => Some(Strong::Left),
        BidiClass::R | BidiClass::AL => Some(Strong::Right),
        _ => None,
    }
}

/// U+FFFE, a noncharacter — a code point that never stands for text — put
/// between the characters that one code of a right-to-left script reads as
/// (a ligature glyph named `uni06440627`, lam-alef, or mapped to two code
/// points by the ToUnicode CMap) when a string is decoded
/// ([`push_glyph_characters`]). A line stored in visual order holds its
/// glyphs in display order but each glyph's characters in the order they
/// were named, so the read-back ([`visual_to_logical`]) has to turn such a
/// glyph round as one cluster: the joiner keeps its characters together
/// (`reverse_clusters`) and is a boundary neutral to the bidirectional
/// algorithm ([`class_of`]). It is dropped once a line's text is in logical
/// order ([`strip_glyph_joiners`]); a character of the document's own is
/// never mistaken for it, since no text holds a noncharacter.
pub(crate) const GLYPH_JOINER: char = '\u{FFFE}';

/// The bidirectional class of `c`. Unicode gives noncharacters the class
/// BN (boundary neutral) as a default, which the `unicode_bidi` tables do
/// not carry: the [`GLYPH_JOINER`] is given it here, so that it is neither
/// a letter nor a strong character to any rule, and takes the level of the
/// character before it as boundary neutrals do.
fn class_of(c: char) -> BidiClass {
    if c == GLYPH_JOINER {
        BidiClass::BN
    } else {
        bidi_class(c)
    }
}

/// Append `label`, what one code reads as, to `out` — with a
/// [`GLYPH_JOINER`] between its characters when there are several and one
/// of them is a right-to-left character.
pub(crate) fn push_glyph_characters(out: &mut String, label: &str) {
    let mut chars = label.chars();
    let several = chars.next().is_some() && chars.next().is_some();
    if !several || !label.chars().any(crate::text_utils::is_rtl_char) {
        out.push_str(label);
        return;
    }
    for (i, ch) in label.chars().enumerate() {
        if i > 0 {
            out.push(GLYPH_JOINER);
        }
        out.push(ch);
    }
}

/// Remove the [`GLYPH_JOINER`]s that [`push_glyph_characters`] put in.
pub(crate) fn strip_glyph_joiners(text: &mut String) {
    if text.contains(GLYPH_JOINER) {
        text.retain(|c| c != GLYPH_JOINER);
    }
}

/// Split `positions` into clusters of a base character followed by its
/// combining marks — and by a [`GLYPH_JOINER`] and the character after it,
/// so that one glyph's characters stay one cluster — and reverse the order
/// of the clusters, keeping each cluster's characters in place. `ch_at`
/// reads the character a position stands for.
fn reverse_clusters(positions: &mut [usize], ch_at: impl Fn(usize) -> char) {
    let mut clusters: Vec<(usize, usize)> = Vec::new();
    for (i, &p) in positions.iter().enumerate() {
        let continues = i > 0
            && (is_combining_mark(ch_at(p))
                || ch_at(p) == GLYPH_JOINER
                || ch_at(positions[i - 1]) == GLYPH_JOINER);
        if !continues {
            clusters.push((i, i + 1));
        } else if let Some(last) = clusters.last_mut() {
            last.1 = i + 1;
        }
    }
    let reordered: Vec<usize> = clusters
        .iter()
        .rev()
        .flat_map(|&(start, end)| positions[start..end].iter().copied())
        .collect();
    positions.copy_from_slice(&reordered);
}

/// Reverse every maximal run of `positions` whose level is at least `min`.
fn reverse_runs_at_least(
    positions: &mut [usize],
    level_at: &[u8],
    min: u8,
    ch_at: impl Fn(usize) -> char,
) {
    let mut start = 0;
    while start < positions.len() {
        if level_at[positions[start]] < min {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < positions.len() && level_at[positions[end]] >= min {
            end += 1;
        }
        reverse_clusters(&mut positions[start..end], &ch_at);
        start = end;
    }
}

/// Un-mirror (or, going the other way, mirror) the characters at odd levels.
fn mirror_at_odd_levels(chars: &mut [char], levels: &[u8]) {
    for (c, &level) in chars.iter_mut().zip(levels) {
        if level % 2 == 1 {
            *c = mirror_char(*c);
        }
    }
}

/// One character of the analysis text: the character the UBA sees and the
/// display position it stands for, `None` for a character inserted to
/// steer the weak rules (dropped from the result).
struct AnalysisChar {
    ch: char,
    position: Option<usize>,
}

/// Resolved embedding level of each display position of `sequence`, read
/// as one paragraph of the given base direction. Paragraph and segment
/// separators (classes B and S) are analyzed as spaces: a line never holds
/// them as content and they would cut the paragraph.
fn resolved_levels(sequence: &[AnalysisChar], rtl_base: bool, out_len: usize) -> Vec<u8> {
    let (level, mark) = if rtl_base {
        (Level::rtl(), '\u{200F}')
    } else {
        (Level::ltr(), '\u{200E}')
    };
    // A direction mark ahead of the text fixes the paragraph direction (rule
    // P2 would otherwise pick it from the first strong character) and stands
    // in for the start-of-text context of the weak and neutral rules.
    let mut text = String::with_capacity(sequence.len() * 2 + 3);
    text.push(mark);
    let is_digit = |c: char| matches!(class_of(c), BidiClass::EN | BidiClass::AN);
    let is_dash =
        |c: char| class_of(c) == BidiClass::ES || matches!(c, '\u{2010}'..='\u{2015}' | '\u{2212}');
    for (i, a) in sequence.iter().enumerate() {
        text.push(match class_of(a.ch) {
            BidiClass::B | BidiClass::S => ' ',
            // The glyph joiner, as a boundary neutral the algorithm's
            // tables know (a zero width space).
            _ if a.ch == GLYPH_JOINER => '\u{200B}',
            // A dash between two digits is part of the number — a range, a
            // date, a phone number — whichever digits they are. Rule W4
            // only joins a European separator to European digits; after
            // an Arabic letter, digits are Arabic numbers and the dash
            // would fall out of the number. Read it as a common separator,
            // which rule W4 joins to either kind.
            _ if is_dash(a.ch)
                && i > 0
                && is_digit(sequence[i - 1].ch)
                && sequence.get(i + 1).is_some_and(|n| is_digit(n.ch)) =>
            {
                ','
            }
            _ => a.ch,
        });
    }
    let info = BidiInfo::new(&text, Some(level));
    let mut out = vec![level.number(); out_len];
    let Some(para) = info.paragraphs.first() else {
        return out;
    };
    let levels = info.reordered_levels_per_char(para, para.range.clone());
    for (a, l) in sequence.iter().zip(levels.iter().skip(1)) {
        if let Some(p) = a.position {
            out[p] = l.number();
        }
    }
    out
}

/// A paired bracket of the display line: the level rule N0 gives it and,
/// when the display alone does not settle it, the other level it could take.
/// `u8::MAX` stands for a pair with no strong character inside, which
/// resolves like the neutrals around it.
struct BracketPair {
    open: usize,
    close: usize,
    level: u8,
    alternative: Option<u8>,
}

/// Paired brackets of `chars` (a line in display order), matched as rule
/// BD16 matches them — on the display line a pair nests properly whether it
/// was written inside a left-to-right phrase or in the right-to-left flow —
/// and placed by the conditions of rule N0 evaluated on the display line.
fn bracket_pairs(chars: &[char], rtl_base: bool) -> Vec<BracketPair> {
    let base_level = u8::from(rtl_base);
    let (base_dir, opposite_dir) = if rtl_base {
        (Strong::Right, Strong::Left)
    } else {
        (Strong::Left, Strong::Right)
    };
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut pairs: Vec<BracketPair> = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        let Some((opening, is_open)) = paired_bracket(c) else {
            continue;
        };
        if is_open {
            stack.push((opening, i));
            continue;
        }
        let Some(depth) = stack.iter().rposition(|&(open, _)| open == opening) else {
            continue;
        };
        let open = stack[depth].1;
        stack.truncate(depth);
        // Rule N0 works on the types the weak rules leave: a number inside
        // a Latin phrase has become L by rule W7, so letters decide. A pair
        // holding only digits reads as R or, after a Latin word, as L; it
        // is left open like a pair of the other direction.
        let mut inner: Vec<Strong> = chars[open + 1..i]
            .iter()
            .filter_map(|&c| letter_direction(c))
            .collect();
        if inner.is_empty()
            && chars[open + 1..i]
                .iter()
                .any(|&c| matches!(class_of(c), BidiClass::EN | BidiClass::AN))
        {
            inner.push(opposite_dir);
        }
        let (level, alternative) = if inner.contains(&base_dir) {
            (base_level, None)
        } else if !inner.is_empty() {
            // A pair enclosing only the other direction takes that direction
            // when the strong character before its opening bracket, in
            // logical order, has it too. For a pair written inside an
            // embedded run that character sits on the run's own reading
            // side of the pair in display order. Whether the pair was
            // written inside the run or around it is what the display may
            // leave open, so the other level stays available.
            let context = if rtl_base {
                chars[..open]
                    .iter()
                    .rev()
                    .find_map(|&c| letter_direction(c))
            } else {
                chars[i + 1..].iter().find_map(|&c| letter_direction(c))
            };
            if context == Some(opposite_dir) {
                (base_level + 1, Some(base_level))
            } else {
                (base_level, Some(base_level + 1))
            }
        } else {
            (u8::MAX, None)
        };
        pairs.push(BracketPair {
            open,
            close: i,
            level,
            alternative,
        });
    }
    pairs
}

/// The direction mark put ahead of a bracket pair read as `level`, so rule
/// N0 resolves the pair that way: the strong character that precedes the
/// pair in logical order. A pair at an odd level belongs to a right-to-left
/// run, whose predecessor is displayed to the right of the closing bracket
/// (an Arabic letter mark when that letter is Arabic, so rule W2 reads the
/// digits inside as it would); a pair at an even level is preceded by
/// Latin text.
fn phantom_before_pair(chars: &[char], pair: &BracketPair, level: u8) -> char {
    if level % 2 == 1 {
        match chars[pair.close + 1..]
            .iter()
            .copied()
            .find_map(|c| letter_class(c).filter(|&k| k != BidiClass::L))
        {
            Some(BidiClass::AL) => '\u{061C}',
            _ => '\u{200F}',
        }
    } else {
        '\u{200E}'
    }
}

/// The class of a letter for the purpose of finding the strong character
/// that precedes a number: `L`, `R` or `AL`. Digits are not letters here.
fn letter_class(c: char) -> Option<BidiClass> {
    match class_of(c) {
        class @ (BidiClass::L | BidiClass::R | BidiClass::AL) => Some(class),
        _ => None,
    }
}

/// The analysis text for one reading of the bracket pairs: the display line
/// read back in the paragraph direction, with `replaced` characters
/// substituted (bracket glyphs mirrored so each pair opens first in the
/// read-back, unpaired brackets neutralized) and `phantoms` — direction
/// marks steering rules N0, W2 and W7 — inserted ahead of the given
/// read-back positions.
///
/// Rules W2 and W7 read a number by the strong letter that precedes it in
/// logical order. Read back, a run of the other direction is reversed, so a
/// number inside such a run is preceded by the letter that logically
/// follows it, and a number displayed right after a Latin word in an RTL
/// paragraph comes before that word. In both cases a direction mark of the
/// letter's class is put ahead of the number, so the weak rules see what
/// the logical line shows them. (The display cannot tell "word number" from
/// "number word" at the edge of a Latin phrase; the reading that keeps the
/// number with the phrase it follows is taken.)
fn analysis_sequence(
    chars: &[char],
    read_back: &[usize],
    rtl_base: bool,
    replaced: &[(usize, char)],
    phantoms: &[(usize, char)],
) -> Vec<AnalysisChar> {
    let class_at = |k: usize| class_of(chars[read_back[k]]);
    // Number tokens as `(start, end)` ranges of the read-back: European
    // digits with the terminators rule W5 joins to them on either side and
    // the single separators rule W4 joins between two digits.
    let mut tokens: Vec<(usize, usize)> = Vec::new();
    let mut k = 0;
    while k < read_back.len() {
        let mut j = k;
        while j < read_back.len() && class_at(j) == BidiClass::ET {
            j += 1;
        }
        if !(j < read_back.len() && class_at(j) == BidiClass::EN) {
            k += 1;
            continue;
        }
        loop {
            while j < read_back.len() && class_at(j) == BidiClass::EN {
                j += 1;
            }
            if j + 1 < read_back.len()
                && matches!(class_at(j), BidiClass::CS | BidiClass::ES)
                && class_at(j + 1) == BidiClass::EN
            {
                j += 1;
                continue;
            }
            while j < read_back.len() && class_at(j) == BidiClass::ET {
                j += 1;
            }
            break;
        }
        tokens.push((k, j));
        k = j;
    }
    // The nearest letter in one direction from a token, skipping digits and
    // neutrals; and the letter touching the token, with nothing but neutrals
    // in between (another number in between makes the token a neighbor of
    // that number, not of the letter).
    let nearest_letter =
        |mut it: Box<dyn Iterator<Item = usize> + '_>| it.find_map(|q| letter_class(chars[q]));
    // A paired bracket is a boundary here: rule N0 makes it strong, and a
    // number inside a pair keeps to the pair.
    let touching_letter = |mut it: Box<dyn Iterator<Item = usize> + '_>| {
        it.find(|&q| {
            paired_bracket(chars[q]).is_some()
                || !matches!(
                    class_of(chars[q]),
                    BidiClass::WS
                        | BidiClass::ON
                        | BidiClass::CS
                        | BidiClass::ES
                        | BidiClass::ET
                        | BidiClass::NSM
                        | BidiClass::BN
                )
        })
        .and_then(|q| letter_class(chars[q]))
    };
    let mut sequence: Vec<AnalysisChar> = Vec::with_capacity(read_back.len() + 4);
    for (k, &p) in read_back.iter().enumerate() {
        for &(_, mark) in phantoms.iter().filter(|(at, _)| *at == k) {
            sequence.push(AnalysisChar {
                ch: mark,
                position: None,
            });
        }
        if let Some(&(_, end)) = tokens.iter().find(|&&(start, _)| start == k) {
            let before = || read_back[..k].iter().rev().copied();
            let after = || read_back[end..].iter().copied();
            let mark = if rtl_base {
                match (
                    touching_letter(Box::new(before())),
                    touching_letter(Box::new(after())),
                ) {
                    // Displayed right after a Latin word: logically after it.
                    (b, Some(BidiClass::L)) if b != Some(BidiClass::L) => Some('\u{200E}'),
                    // Displayed right before a Latin word: the start of that
                    // phrase, which only rule W7 — Latin text before the
                    // number — can put there.
                    (Some(BidiClass::L), a) if a != Some(BidiClass::L) => Some('\u{200E}'),
                    _ => None,
                }
            } else {
                // Inside a right-to-left run: preceded, logically, by the
                // letter that follows it in the read-back.
                match (
                    nearest_letter(Box::new(before())),
                    nearest_letter(Box::new(after())),
                ) {
                    (Some(b), Some(a)) if b != BidiClass::L && a != BidiClass::L => {
                        Some(if a == BidiClass::AL {
                            '\u{061C}'
                        } else {
                            '\u{200F}'
                        })
                    }
                    _ => None,
                }
            };
            if let Some(mark) = mark {
                sequence.push(AnalysisChar {
                    ch: mark,
                    position: None,
                });
            }
        }
        let ch = match replaced.iter().find(|(q, _)| *q == p) {
            Some(&(_, replacement)) => replacement,
            None => chars[p],
        };
        sequence.push(AnalysisChar {
            ch,
            position: Some(p),
        });
    }
    sequence
}

/// Undo rule L2 for `level_at` (levels indexed by display position): the
/// reversals it applied from the highest level down to the lowest odd
/// level, applied from the lowest level upwards. Returns the display
/// positions in logical order together with the characters, un-mirrored
/// where they sat at an odd level. (At an RTL base every level is at least
/// 1, so the first pass reverses the whole line.)
fn undo_reordering(chars: &[char], level_at: &[u8]) -> (Vec<usize>, Vec<char>) {
    let max_level = level_at.iter().copied().max().unwrap_or(0);
    let mut logical: Vec<usize> = (0..chars.len()).collect();
    for min in 1..=max_level {
        reverse_runs_at_least(&mut logical, level_at, min, |p| chars[p]);
    }
    let mut out_chars: Vec<char> = logical.iter().map(|&p| chars[p]).collect();
    let out_levels: Vec<u8> = logical.iter().map(|&p| level_at[p]).collect();
    mirror_at_odd_levels(&mut out_chars, &out_levels);
    (logical, out_chars)
}

/// Deepest bracket nesting matched, as in rule BD16: a line that opens more
/// brackets than this without closing them is not scanned further. It also
/// bounds the work per character, so matching is linear in the line.
const MAX_BRACKET_DEPTH: usize = 63;

/// Bracket pairs of `chars` as `(left, right)` display positions. Pairs
/// match as rule BD16 has them, an opening bracket first, or with
/// `backwards` a closing bracket first and its opening bracket further
/// right. Positions for which `skip` is true are left out.
fn match_brackets(
    chars: &[char],
    backwards: bool,
    skip: impl Fn(usize) -> bool,
) -> Vec<(usize, usize)> {
    let mut starts: Vec<(char, usize)> = Vec::new();
    let mut found = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        let Some((opening, is_open)) = paired_bracket(c) else {
            continue;
        };
        if skip(i) {
            continue;
        }
        if is_open != backwards {
            if starts.len() == MAX_BRACKET_DEPTH {
                break;
            }
            starts.push((opening, i));
        } else if let Some(depth) = starts.iter().rposition(|&(open, _)| open == opening) {
            found.push((starts[depth].1, i));
            starts.truncate(depth);
        }
    }
    found
}

/// Which of `len` positions are an end of one of `pairs`.
fn mark_pairs(len: usize, pairs: &[(usize, usize)]) -> Vec<bool> {
    let mut marked = vec![false; len];
    for &(left, right) in pairs {
        marked[left] = true;
        marked[right] = true;
    }
    marked
}

/// Whether a bracket pair around `inside` reads `(…)` whichever way the page
/// stores its brackets: it holds left-to-right letters.
fn encloses_latin(inside: &[char]) -> bool {
    inside
        .iter()
        .any(|&c| letter_direction(c) == Some(Strong::Left))
}

/// Whether a bracket pair around `inside` is drawn mirrored on a page that
/// stores its brackets by role, so that it reads `(…)` there: it holds
/// right-to-left letters or digits (and, checked first, no Latin letter).
fn encloses_right_to_left(inside: &[char]) -> bool {
    inside
        .iter()
        .any(|&c| letter_direction(c) == Some(Strong::Right) || c.is_numeric())
}

/// The display position of a list marker's bracket on a right-to-left
/// line: a bracket followed on screen by nothing but a number, as `1)`
/// shows at the start of the line, which is its right end.
fn list_marker_bracket(chars: &[char]) -> Option<usize> {
    let end = chars.iter().rposition(|c| !c.is_whitespace())?;
    let digits = chars[..=end]
        .iter()
        .rev()
        .take_while(|c| c.is_numeric())
        .count();
    let at = end.checked_sub(digits)?;
    (digits > 0 && paired_bracket(chars[at]).is_some()).then_some(at)
}

/// Display positions of brackets stored under the character they were typed
/// as, so that a pair reads `)…(` on screen although its glyphs are drawn
/// mirrored.
///
/// A right-to-left line that shows its pairs mirrored reads `(…)` on screen.
/// Some producers (InDesign's Middle Eastern edition among them) store the
/// bracket glyph a pair takes in right-to-left text under the character it
/// was typed as, so the screen line holds `)…(` and the characters are
/// already the logical ones: rule L4 must not be undone for them. A line is
/// read that way when more of its brackets pair up inverted than nested, or
/// as many with tighter pairs: in `)a( b )c(` the middle `( b )` nests but
/// the two inverted pairs fit better. A list marker's bracket (`1)` at the
/// start of the line) that has no partner of its own is no part of an
/// inverted pair: `1) ו (שלום)` keeps its nested reading.
fn inverted_bracket_positions(chars: &[char], shown: &[bool]) -> Vec<bool> {
    let shown_at = |i: usize| shown.get(i).copied().unwrap_or(false);
    let nested = match_brackets(chars, false, shown_at);
    let nested_marks = mark_pairs(chars.len(), &nested);
    let leftover = match_brackets(chars, true, |i| shown_at(i) || nested_marks[i]);
    let leftover_marks = mark_pairs(chars.len(), &leftover);
    // A bracket that finds a partner among the leftovers is a pair's own,
    // `)א(2` stored by role, and no list marker.
    let marker = list_marker_bracket(chars).filter(|&at| !leftover_marks[at]);
    let all = match_brackets(chars, true, |i| shown_at(i) || marker == Some(i));
    let span = |found: &[(usize, usize)]| found.iter().map(|&(l, r)| r - l).sum::<usize>();
    let mixed_count = nested.len() + leftover.len();
    let mixed_span = span(&nested) + span(&leftover);
    let all_wins = all.len() > mixed_count || (all.len() == mixed_count && span(&all) < mixed_span);
    mark_pairs(chars.len(), if all_wins { &all } else { &leftover })
}

/// Whether a page stores its brackets under the character they were typed
/// as, judged from its right-to-left lines in screen order (`lines`, each
/// line's characters left to right).
///
/// A line alone cannot tell. `)אב גד(` is a pair stored by role, and it is
/// just as well the end of one parenthetical and the start of the next on a
/// page that draws its pairs mirrored, each continued on a neighbouring
/// line. Either way of storing makes such false pairs only where a
/// parenthetical wraps, while every pair kept on one line shows the page's
/// own way: `)…(` stored by role, `(…)` drawn mirrored. So the page counts
/// as storing by role when more of its pairs read `)…(` than `(…)`. Pairs
/// around Latin text read `(…)` whichever way the page stores brackets, so
/// they say nothing and their brackets are not paired backwards with
/// others: `(ABC) א (DEF)` has no backwards pair. Only pairs around
/// right-to-left letters or digits count as drawn.
pub(crate) fn brackets_stored_by_role<'a>(lines: impl IntoIterator<Item = &'a [char]>) -> bool {
    let mut backwards = 0;
    let mut drawn = 0;
    for chars in lines {
        let mut held = vec![false; chars.len()];
        for (open, close) in match_brackets(chars, false, |_| false) {
            let inside = &chars[open + 1..close];
            if encloses_latin(inside) {
                held[open] = true;
                held[close] = true;
            } else if encloses_right_to_left(inside) {
                drawn += 1;
            }
        }
        backwards += match_brackets(chars, true, |i| held[i]).len();
    }
    backwards > drawn
}

/// Most bracket pairs whose level is left open by the display that are
/// tried in combination; beyond that the rule N0 reading stands.
const MAX_OPEN_PAIRS: u32 = 4;

/// The combinations of levels for the pairs the display leaves open, as bit
/// masks over `open_pairs` pairs, those that flip the fewest first.
fn level_readings(open_pairs: usize) -> Vec<u32> {
    if open_pairs as u32 > MAX_OPEN_PAIRS {
        return vec![0];
    }
    let mut masks: Vec<u32> = (0..1u32 << open_pairs).collect();
    masks.sort_by_key(|mask| mask.count_ones());
    masks
}

/// Brackets in the analysis text: read back in an RTL paragraph a pair
/// closes first, so both glyphs are mirrored to let rule BD16 match them; a
/// bracket without a partner is neutralized (a broken bar has its class,
/// ON, and no partner to pair with).
fn analysis_brackets(chars: &[char], pairs: &[BracketPair], rtl_base: bool) -> Vec<(usize, char)> {
    let paired = mark_pairs_of(chars.len(), pairs);
    let mut replaced: Vec<(usize, char)> = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        if paired_bracket(c).is_none() {
            continue;
        }
        if !paired[i] {
            replaced.push((i, '\u{00A6}'));
        } else if rtl_base {
            replaced.push((i, mirror_char(c)));
        }
    }
    replaced
}

/// Which display positions are an end of one of `pairs`.
fn mark_pairs_of(len: usize, pairs: &[BracketPair]) -> Vec<bool> {
    let ends: Vec<(usize, usize)> = pairs.iter().map(|pair| (pair.open, pair.close)).collect();
    mark_pairs(len, &ends)
}

/// The marks that steer the pairs the display leaves open to the levels
/// `mask` picks, as `(read-back index, mark)`. Only a pair the display
/// leaves open needs steering; a pair fixed by its content resolves by rule
/// N0 as it stands, and a mark ahead of it would shadow the letter rules W2
/// and W7 read through it.
fn steering_marks(
    chars: &[char],
    pairs: &[BracketPair],
    open_pairs: &[usize],
    mask: u32,
    rtl_base: bool,
    read_back_index: &[usize],
) -> Vec<(usize, char)> {
    let mut phantoms: Vec<(usize, char)> = Vec::new();
    for (i, pair) in pairs.iter().enumerate() {
        let Some(alternative) = pair.alternative else {
            continue;
        };
        let flipped = open_pairs
            .iter()
            .position(|&open| open == i)
            .is_some_and(|bit| bit < MAX_OPEN_PAIRS as usize && mask & (1 << bit) != 0);
        let level = if flipped { alternative } else { pair.level };
        // The mark goes right before the bracket that opens the pair in
        // the read-back: the closing glyph in an RTL paragraph.
        let opener = if rtl_base { pair.close } else { pair.open };
        phantoms.push((
            read_back_index[opener],
            phantom_before_pair(chars, pair, level),
        ));
    }
    phantoms
}

/// `chars` with the brackets that were stored under the character they were
/// typed as swapped to the glyph a mirrored display shows, so that the line
/// reads like one from a page that draws its pairs mirrored (see
/// [`inverted_bracket_positions`]).
fn drawn_mirrored(chars: &[char], shown: &[bool]) -> Vec<char> {
    let inverted = inverted_bracket_positions(chars, shown);
    chars
        .iter()
        .zip(inverted)
        .map(|(&c, swap)| if swap { mirror_char(c) } else { c })
        .collect()
}

/// The logical (reading) order of a line given in screen order.
///
/// `visual` holds the line's characters from left to right as they were
/// painted, `rtl_base` says which way the paragraph reads. The result holds
/// the same characters in logical order — paired brackets un-mirrored where
/// the display had mirrored them — with each character's item tag.
///
/// `brackets_by_role` is `Some` when the page stores its brackets under the
/// character they were typed as (see [`brackets_stored_by_role`]), with a
/// flag per display position for the characters that are exempt, glyphs
/// shown as they are (see [`LineDirection::shown_items`]); only then are
/// backwards pairs on a right-to-left line paired as they stand: they are
/// read as the mirrored pairs they stand for before anything else is
/// decided.
///
/// Where a bracket pair could have been written at either of two levels,
/// each reading is rendered back to display order and the first one that
/// reproduces the line is taken, the rule N0 reading first.
pub(crate) fn visual_to_logical(
    visual: &[VisualChar],
    rtl_base: bool,
    brackets_by_role: Option<&[bool]>,
) -> Vec<VisualChar> {
    if visual.is_empty() {
        return Vec::new();
    }
    let mut chars: Vec<char> = visual.iter().map(|v| v.ch).collect();
    if let (true, Some(shown)) = (rtl_base, brackets_by_role) {
        chars = drawn_mirrored(&chars, shown);
    }

    // The display line read back in the paragraph direction: the reading
    // order of everything but the embedded runs of the other direction.
    let mut read_back: Vec<usize> = (0..chars.len()).collect();
    if rtl_base {
        reverse_clusters(&mut read_back, |p| chars[p]);
    }

    let pairs = bracket_pairs(&chars, rtl_base);
    let open_pairs: Vec<usize> = pairs
        .iter()
        .enumerate()
        .filter_map(|(i, pair)| pair.alternative.map(|_| i))
        .collect();

    // Where each display position sits in the read-back.
    let mut read_back_index = vec![0usize; chars.len()];
    for (k, &p) in read_back.iter().enumerate() {
        read_back_index[p] = k;
    }
    let replaced = analysis_brackets(&chars, &pairs, rtl_base);

    let mut first: Option<(Vec<usize>, Vec<char>)> = None;
    for mask in level_readings(open_pairs.len()) {
        let phantoms = steering_marks(
            &chars,
            &pairs,
            &open_pairs,
            mask,
            rtl_base,
            &read_back_index,
        );
        let sequence = analysis_sequence(&chars, &read_back, rtl_base, &replaced, &phantoms);
        let level_at = resolved_levels(&sequence, rtl_base, chars.len());
        let (logical, out_chars) = undo_reordering(&chars, &level_at);
        if display_order(&out_chars, rtl_base) == chars {
            first = Some((logical, out_chars));
            break;
        }
        if first.is_none() {
            first = Some((logical, out_chars));
        }
    }
    let (logical, out_chars) = first.unwrap_or_default();
    logical
        .iter()
        .zip(out_chars)
        .map(|(&p, ch)| VisualChar {
            ch,
            item: visual[p].item,
        })
        .collect()
}

/// `chars`, a line in logical order, as a paragraph of the given base
/// direction displays it: rule L2 reordering with combining marks kept after
/// their base (rule L3) and the characters at odd levels mirrored (rule L4).
fn display_order(chars: &[char], rtl_base: bool) -> Vec<char> {
    if chars.is_empty() {
        return Vec::new();
    }
    let sequence: Vec<AnalysisChar> = chars
        .iter()
        .enumerate()
        .map(|(p, &ch)| AnalysisChar {
            ch,
            position: Some(p),
        })
        .collect();
    let level_at = resolved_levels(&sequence, rtl_base, chars.len());
    let max_level = level_at.iter().copied().max().unwrap_or(0);
    let mut display: Vec<usize> = (0..chars.len()).collect();
    for min in (1..=max_level).rev() {
        reverse_runs_at_least(&mut display, &level_at, min, |p| chars[p]);
    }
    let mut out: Vec<char> = display.iter().map(|&p| chars[p]).collect();
    let levels: Vec<u8> = display.iter().map(|&p| level_at[p]).collect();
    mirror_at_odd_levels(&mut out, &levels);
    out
}

/// `text`, logical order, as a paragraph of the given base direction is
/// displayed (see [`display_order`]).
pub(crate) fn logical_to_visual(text: &str, rtl_base: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    display_order(&chars, rtl_base).into_iter().collect()
}

/// Whether a character is a Hebrew or Arabic presentation form: a
/// positional or ligated glyph variant with its own code point, which a font
/// that was subset by glyph maps its codes to instead of the letters. Not
/// U+FEFF, the byte order mark that shares the Arabic block.
pub(crate) fn is_presentation_form(c: char) -> bool {
    matches!(c, '\u{FB1D}'..='\u{FB4F}' | '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFE}')
}

/// `text` with every presentation form replaced by the letters it stands
/// for (its compatibility decomposition), leaving all other characters
/// untouched. Applied once the text is in logical order, so a ligature's
/// letters come out in reading order.
pub(crate) fn normalize_presentation_forms(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_presentation_form) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_presentation_form(c) {
            out.extend(std::iter::once(c).nfkc());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Gap, in em, between two items of one line from which a word space is
/// assumed to separate them when the line is read back for reordering.
const WORD_GAP_EM: f32 = 0.1;

/// Gap, in em, from which a word space is assumed between two items that
/// meet with digits or the separators of a number: a hyphen or a decimal
/// point set a little apart from its digits must not cut the number in two
/// before the weak rules see it.
const NUMBER_GAP_EM: f32 = 0.3;

/// Whether `c` can sit inside a number: a digit or one of the separators
/// and terminators rules W4 and W5 join to digits.
fn joins_number(c: char) -> bool {
    matches!(
        class_of(c),
        BidiClass::EN | BidiClass::AN | BidiClass::ES | BidiClass::CS | BidiClass::ET
    )
}

/// How a line reads: its paragraph direction, and whether its page stores
/// brackets under the character they were typed as (see
/// [`brackets_stored_by_role`]).
#[derive(Debug, Clone, Copy)]
pub(crate) struct LineDirection<'a> {
    pub(crate) rtl_base: bool,
    pub(crate) brackets_by_role: bool,
    /// Items, by index, whose brackets are the glyphs a display shows
    /// whatever the page stores (ActualText replacements, rendered by
    /// [`logical_to_visual`]): the page's way of storing is not applied to
    /// them. Items past the end of the slice are not exempt.
    pub(crate) shown_items: &'a [bool],
}

/// The logical order of a line's items.
///
/// `items` are one line's items in screen order (ascending `x`);
/// `text_is_visual(i)` says whether item `i`'s text is in display order
/// (decoded from glyphs painted in visual order) or already logical. Every
/// item's text is rendered in display order where needed, the display line
/// is inverted with [`visual_to_logical`], and the line's characters are
/// handed back to the items: the result lists `(item index, logical text)`
/// in reading order. `direction` gives the paragraph direction and the
/// page's bracket storage (see [`visual_to_logical`]).
///
/// A word gap between two items in screen order puts a space into the
/// display line for the analysis: `word_gap` is the line's own floor for
/// one, in em of the smaller font beside the gap, when its gaps yield one
/// (a line shown one glyph per item, see `glyph_run_word_gap_floor`);
/// otherwise a gap is a word gap from a tenth of an em. Between the two halves of a number it takes
/// three tenths whatever the floor — digits keep their own widths where a
/// font's letters are off, and a kerned or split number is still one
/// token, which a space would break into two numbers the paragraph
/// direction then orders.
///
/// Items holding a letter or a digit are ordered by where those characters
/// land in the logical line. An item of punctuation alone keeps the
/// neighbours it has on the page — a bracket glyph may be either of the
/// two characters that mirror to it, so its place in the text is not its
/// own to tell — and a run of such items between two lettered items sits
/// between the same two items in the reading order, turned round with
/// them when they are. Each item then takes the next stretch of the
/// logical line, as long as its own text, so a bracket pair split across
/// runs — `(IFRS` and `16)` painted as separate words — keeps both runs
/// whole. An item of combining marks alone — a point or a vowel sign shown
/// apart from its letter, over the lettered item before it in screen order
/// and inside that item's advance — is that letter's own characters and
/// follows it in the reading, whichever way the stretch reads. Items
/// without a single character (empty text) follow the others
/// in screen order.
pub(crate) fn logical_line_order<T>(
    items: &[T],
    text_of: impl Fn(&T) -> &str,
    span_of: impl Fn(&T) -> (f32, f32),
    em_of: impl Fn(&T) -> f32,
    text_is_visual: impl Fn(usize) -> bool,
    word_gap: Option<f32>,
    direction: LineDirection,
) -> Vec<(usize, String)> {
    let LineDirection {
        rtl_base,
        brackets_by_role,
        shown_items,
    } = direction;
    let mut visual: Vec<VisualChar> = Vec::new();
    let mut shown: Vec<bool> = Vec::new();
    let mut counts: Vec<usize> = vec![0; items.len()];
    let mut prev_right: Option<f32> = None;
    let mut prev_last: Option<char> = None;
    let mut prev_em = f32::INFINITY;
    for (index, item) in items.iter().enumerate() {
        let (x, width) = span_of(item);
        let display: Cow<'_, str> = if text_is_visual(index) {
            Cow::Borrowed(text_of(item))
        } else {
            Cow::Owned(logical_to_visual(text_of(item), rtl_base))
        };
        if let Some(right) = prev_right {
            let numeric_junction = prev_last.is_some_and(joins_number)
                && display.chars().next().is_some_and(joins_number);
            let floor = match word_gap {
                _ if numeric_junction => em_of(item).max(1.0) * NUMBER_GAP_EM,
                Some(floor_em) => floor_em * em_of(item).min(prev_em).max(1.0),
                None => em_of(item).max(1.0) * WORD_GAP_EM,
            };
            if x - right > floor {
                visual.push(VisualChar {
                    ch: ' ',
                    item: None,
                });
                shown.push(false);
            }
        }
        prev_right = Some(prev_right.unwrap_or(f32::MIN).max(x + width.max(0.0)));
        prev_last = display.chars().last().or(prev_last);
        prev_em = em_of(item);
        counts[index] = display.chars().count();
        visual.extend(display.chars().map(|ch| VisualChar {
            ch,
            item: Some(index),
        }));
        shown.resize(visual.len(), shown_items.get(index) == Some(&true));
    }

    let logical = visual_to_logical(
        &visual,
        rtl_base,
        brackets_by_role.then_some(shown.as_slice()),
    );
    // Where each item's letters and digits land in the logical line.
    let is_strong = |c: char| c.is_alphanumeric() && !is_combining_mark(c);
    let mut strong_positions: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    for (at, VisualChar { ch, item }) in logical.iter().enumerate() {
        if let Some(index) = item {
            if is_strong(*ch) {
                strong_positions[*index].push(at);
            }
        }
    }
    let mut reading: Vec<usize> = (0..items.len())
        .filter(|&i| !strong_positions[i].is_empty())
        .collect();
    reading.sort_by_key(|&i| strong_positions[i][strong_positions[i].len() / 2]);
    let lettered = |i: usize| !strong_positions[i].is_empty();

    // Runs of punctuation-only items, placed by their neighbours.
    let mut k = 0;
    while k < items.len() {
        if counts[k] == 0 || lettered(k) {
            k += 1;
            continue;
        }
        let run_start = k;
        while k < items.len() && !lettered(k) {
            k += 1;
        }
        let mut run: Vec<usize> = (run_start..k).filter(|&i| counts[i] > 0).collect();
        let left = (0..run_start).rev().find(|&i| lettered(i));
        let right = (k..items.len()).find(|&i| lettered(i));
        // Combining marks shown as items of their own over the lettered
        // item to their left, inside its advance, are that letter's own
        // characters and follow it in the reading, whichever way the
        // stretch reads; the rest of the run is placed by its neighbours.
        if let Some(l) = left {
            let (left_x, left_width) = span_of(&items[l]);
            let over_left = |i: usize| {
                let (x, _) = span_of(&items[i]);
                text_of(&items[i]).chars().all(is_combining_mark)
                    && x >= left_x.min(left_x + left_width)
                    && x < left_x.max(left_x + left_width)
            };
            let marks = run.iter().take_while(|&&i| over_left(i)).count();
            if marks > 0 {
                let at = reading.iter().position(|&r| r == l).map_or(0, |p| p + 1);
                for (offset, i) in run.drain(..marks).enumerate() {
                    reading.insert(at + offset, i);
                }
                if run.is_empty() {
                    continue;
                }
            }
        }
        let place = |i: usize| reading.iter().position(|&r| r == i).unwrap_or(0);
        let (insert_at, reversed) = match (left, right) {
            // Between two lettered items: right after the one read first,
            // turned round when the stretch reads right to left.
            (Some(l), Some(r)) => {
                let (pl, pr) = (place(l), place(r));
                if pl < pr {
                    (pl + 1, false)
                } else {
                    (pr + 1, true)
                }
            }
            // At the page's left edge: the start of the reading when the
            // neighbour is read first, else its very end, turned round.
            (None, Some(r)) => {
                if place(r) == 0 {
                    (0, false)
                } else {
                    (reading.len(), true)
                }
            }
            // At the page's right edge: the end of the reading when the
            // neighbour is read last, else its very start, turned round.
            (Some(l), None) => {
                if place(l) + 1 == reading.len() {
                    (reading.len(), false)
                } else {
                    (0, true)
                }
            }
            (None, None) => (0, rtl_base),
        };
        if reversed {
            run.reverse();
        }
        for (offset, i) in run.into_iter().enumerate() {
            reading.insert(insert_at + offset, i);
        }
    }

    let mut chars = logical.iter().filter(|v| v.item.is_some()).map(|v| v.ch);
    let mut result: Vec<(usize, String)> = reading
        .into_iter()
        .map(|index| (index, chars.by_ref().take(counts[index]).collect()))
        .collect();
    result.extend(
        (0..items.len())
            .filter(|&i| counts[i] == 0)
            .map(|i| (i, String::new())),
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visual(text: &str) -> Vec<VisualChar> {
        text.chars()
            .map(|ch| VisualChar { ch, item: None })
            .collect()
    }

    fn logical_text(display: &str, rtl_base: bool) -> String {
        visual_to_logical(&visual(display), rtl_base, None)
            .into_iter()
            .map(|v| v.ch)
            .collect()
    }

    /// As [`logical_text`] for a page whose brackets are stored by role.
    fn logical_text_by_role(display: &str, rtl_base: bool) -> String {
        visual_to_logical(&visual(display), rtl_base, Some(&[]))
            .into_iter()
            .map(|v| v.ch)
            .collect()
    }

    // שלום עולם
    const HELLO_WORLD: &str = "\u{05E9}\u{05DC}\u{05D5}\u{05DD} \u{05E2}\u{05D5}\u{05DC}\u{05DD}";

    #[test]
    fn mirrored_characters_come_from_the_unicode_data() {
        // Mathematical white square brackets around a Hebrew word: the
        // display shows the opening glyph at the left, and the pair reads
        // back as it was written.
        assert_eq!(
            logical_text("\u{27E6}\u{05D1}\u{05D0}\u{27E7}", true),
            "\u{27E6}\u{05D0}\u{05D1}\u{27E7}"
        );
        // A mirrored character that pairs with nothing (less-than or
        // equal to) is un-mirrored by the same data.
        assert_eq!(
            logical_text("\u{05D1} \u{2265} \u{05D0}", true),
            "\u{05D0} \u{2264} \u{05D1}"
        );
        assert_eq!(mirror_char('\u{00AB}'), '\u{00BB}');
        assert_eq!(mirror_char('a'), 'a');
        // Canonically equivalent angle brackets pair with each other.
        assert_eq!(paired_bracket('\u{2329}'), Some(('\u{3008}', true)));
        assert_eq!(paired_bracket('\u{3009}'), Some(('\u{3008}', false)));
        assert_eq!(paired_bracket('\u{00AB}'), None);
    }

    #[test]
    fn the_glyph_joiner_is_transparent_to_the_embedding_levels() {
        let levels = |text: &str, rtl_base: bool| -> Vec<u8> {
            let chars: Vec<char> = text.chars().collect();
            let sequence: Vec<AnalysisChar> = chars
                .iter()
                .enumerate()
                .map(|(p, &ch)| AnalysisChar {
                    ch,
                    position: Some(p),
                })
                .collect();
            resolved_levels(&sequence, rtl_base, chars.len())
        };
        assert_eq!(class_of(GLYPH_JOINER), BidiClass::BN);
        // a لاه 12 b, with the joiner inside the lam-alef: in either
        // paragraph direction the joiner takes the level of the letter
        // before it and changes no other level.
        let plain = "a \u{0644}\u{0627}\u{0647} 12 b";
        let joined = format!("a \u{0644}{GLYPH_JOINER}\u{0627}\u{0647} 12 b");
        for rtl_base in [true, false] {
            let expected = levels(plain, rtl_base);
            let mut with_joiner = levels(&joined, rtl_base);
            let joiner = with_joiner.remove(3);
            assert_eq!(with_joiner, expected, "rtl_base {rtl_base}");
            assert_eq!(joiner, expected[2], "rtl_base {rtl_base}");
        }
    }

    #[test]
    fn the_characters_of_one_glyph_stay_together_through_the_read_back() {
        // A lam-alef glyph decoded as its two letters, joined: turned round
        // as one cluster, it keeps the order its letters were named in.
        let mut lam_alef = String::new();
        push_glyph_characters(&mut lam_alef, "\u{0644}\u{0627}");
        assert_eq!(lam_alef, format!("\u{0644}{GLYPH_JOINER}\u{0627}"));
        // Displayed left to right: the lam-alef glyph, then heh — the word
        // heh, lam, alef.
        let display = format!("{lam_alef}\u{0647}");
        let logical = format!("\u{0647}{lam_alef}");
        assert_eq!(logical_text(&display, true), logical);
        assert_eq!(logical_to_visual(&logical, true), display);
        // Embedded in a left-to-right paragraph as well.
        assert_eq!(
            logical_text(&format!("a {display} b"), false),
            format!("a {logical} b")
        );
        // Without the joiner the two letters change places.
        assert_eq!(
            logical_text("\u{0644}\u{0627}\u{0647}", true),
            "\u{0647}\u{0627}\u{0644}"
        );
        // Latin ligature letters, a fraction and single characters are not
        // joined.
        for label in ["fi", "1/2", "\u{0627}", "a", ""] {
            let mut out = String::new();
            push_glyph_characters(&mut out, label);
            assert_eq!(out, label);
        }
        let mut text = logical.clone();
        strip_glyph_joiners(&mut text);
        assert_eq!(text, "\u{0647}\u{0644}\u{0627}");
    }

    #[test]
    fn pure_rtl_line_is_reversed() {
        let display = logical_to_visual(HELLO_WORLD, true);
        assert_eq!(display, HELLO_WORLD.chars().rev().collect::<String>());
        assert_eq!(logical_text(&display, true), HELLO_WORLD);
    }

    #[test]
    fn embedded_number_and_trailing_colon_keep_their_places() {
        // תרשים 4:
        let logical = "\u{05EA}\u{05E8}\u{05E9}\u{05D9}\u{05DD} 42:";
        let display = logical_to_visual(logical, true);
        assert_eq!(display, ":42 \u{05DD}\u{05D9}\u{05E9}\u{05E8}\u{05EA}");
        assert_eq!(logical_text(&display, true), logical);
    }

    #[test]
    fn latin_phrase_with_its_own_brackets_stays_whole() {
        // אב ABC (DEF) גד
        let logical = "\u{05D0}\u{05D1} ABC (DEF) \u{05D2}\u{05D3}";
        let display = logical_to_visual(logical, true);
        assert_eq!(display, "\u{05D3}\u{05D2} ABC (DEF) \u{05D1}\u{05D0}");
        assert_eq!(logical_text(&display, true), logical);
    }

    #[test]
    fn brackets_in_the_rtl_flow_are_unmirrored() {
        // אב (IASB) גד — the pair takes the paragraph direction and is
        // drawn mirrored, so the display holds the mirrored glyphs.
        let logical = "\u{05D0}\u{05D1} (IASB) \u{05D2}\u{05D3}";
        let display = logical_to_visual(logical, true);
        assert_eq!(display, "\u{05D3}\u{05D2} (IASB) \u{05D1}\u{05D0}");
        assert_eq!(logical_text(&display, true), logical);
        // Brackets around RTL text likewise.
        let logical = "\u{05D0} (\u{05D1}\u{05D2}) \u{05D3}";
        assert_eq!(
            logical_text(&logical_to_visual(logical, true), true),
            logical
        );
    }

    #[test]
    fn brackets_stored_by_logical_role_are_not_mirrored_again() {
        // The screen line holds `)1(`: the producer stored each bracket
        // under the character it was typed as, so the characters are the
        // logical ones already and reading them back must not swap them.
        // Arabic "الفقرة (1) من" and Hebrew "אב (1) גד".
        let display = "\u{0646}\u{0645} )1( \u{0629}\u{0631}\u{0642}\u{0641}\u{0644}\u{0627}";
        assert_eq!(
            logical_text_by_role(display, true),
            "\u{0627}\u{0644}\u{0641}\u{0642}\u{0631}\u{0629} (1) \u{0645}\u{0646}"
        );
        let display = "\u{05D3}\u{05D2} )1( \u{05D1}\u{05D0}";
        assert_eq!(
            logical_text_by_role(display, true),
            "\u{05D0}\u{05D1} (1) \u{05D2}\u{05D3}"
        );
        // Around a word, and the heading-style pair at the end of a line.
        let display = "\u{05D3}\u{05D2} )\u{05D1}\u{05D0}( \u{05D5}";
        assert_eq!(
            logical_text_by_role(display, true),
            "\u{05D5} (\u{05D0}\u{05D1}) \u{05D2}\u{05D3}"
        );
    }

    #[test]
    fn brackets_already_drawn_mirrored_are_not_swapped() {
        // The screen line holds `(1)` — mirrored glyphs, as a shaping
        // engine draws them — and the logical text is `(1)` too. Reading
        // it back must give the same pair, not `)1(`.
        let display = "\u{05D3}\u{05D2} (1) \u{05D1}\u{05D0}";
        assert_eq!(
            logical_text(display, true),
            "\u{05D0}\u{05D1} (1) \u{05D2}\u{05D3}"
        );
        // Two pairs side by side, as in "الفقرة (1) و(2)".
        let display = "(2) \u{05D5}(1) \u{05D1}\u{05D0}";
        assert_eq!(
            logical_text(display, true),
            "\u{05D0}\u{05D1} (1)\u{05D5} (2)"
        );
    }

    #[test]
    fn several_inverted_pairs_on_a_line_are_each_read_as_a_pair() {
        // "الفقرة (1) من المادة (الخامسة)": the `(` ending the first pair
        // and the `)` opening the second nest on screen, but they are not
        // a pair.
        let display = ")\u{0629}\u{0633}\u{0645}\u{0627}\u{062E}\u{0644}\u{0627}( \u{0629}\u{062F}\u{0627}\u{0645}\u{0644}\u{0627} \u{0646}\u{0645} )1( \u{0629}\u{0631}\u{0642}\u{0641}\u{0644}\u{0627}";
        assert_eq!(
            logical_text_by_role(display, true),
            "\u{0627}\u{0644}\u{0641}\u{0642}\u{0631}\u{0629} (1) \u{0645}\u{0646} \u{0627}\u{0644}\u{0645}\u{0627}\u{062F}\u{0629} (\u{0627}\u{0644}\u{062E}\u{0627}\u{0645}\u{0633}\u{0629})"
        );
    }

    #[test]
    fn mixed_direction_lines_keep_their_latin_brackets() {
        // A Latin pair inside an RTL line is nested on screen and stays
        // that way, while a pair stored by logical role beside it is read
        // as one.
        let display = "\u{05D3}\u{05D2} (IASB) \u{05D1}\u{05D0} )1( \u{05D0}";
        assert_eq!(
            logical_text_by_role(display, true),
            "\u{05D0} (1) \u{05D0}\u{05D1} (IASB) \u{05D2}\u{05D3}"
        );
        // A left-to-right paragraph is untouched.
        assert_eq!(logical_text_by_role(")1( abc", false), ")1( abc");
    }

    fn page_stores_by_role(texts: &[&str]) -> bool {
        let lines: Vec<Vec<char>> = texts.iter().map(|t| t.chars().collect()).collect();
        brackets_stored_by_role(lines.iter().map(Vec::as_slice))
    }

    #[test]
    fn unrelated_brackets_on_a_mirrored_page_are_unswapped_one_by_one() {
        // `)א(` on a page that draws its pairs mirrored: the `(` closes a
        // parenthetical from the line before, the `)` opens one going on
        // to the next. The page's other pair reads `(1)`, so the line is
        // read as before, each glyph mirrored back on its own.
        let page = [")\u{05D0}(", "\u{05D3}\u{05D2} (1) \u{05D1}\u{05D0}"];
        assert!(!page_stores_by_role(&page));
        assert_eq!(logical_text(page[0], true), ")\u{05D0}(");
    }

    #[test]
    fn parentheticals_wrapping_in_and_out_of_a_line_stay_apart() {
        // `)דג בא(` on a mirrored page is the fragment `)אב גד(`: the end
        // of one parenthetical and the start of the next, not a pair.
        let page = [
            ")\u{05D3}\u{05D2} \u{05D1}\u{05D0}(",
            "(\u{05D5}) \u{05D4} (\u{05D6})",
        ];
        assert!(!page_stores_by_role(&page));
        assert_eq!(
            logical_text(page[0], true),
            ")\u{05D0}\u{05D1} \u{05D2}\u{05D3}("
        );
    }

    #[test]
    fn a_list_marker_is_not_paired_with_a_bracket_of_a_later_pair() {
        // `1) ו (שלום)` drawn mirrored: the list marker's bracket and the
        // `(` before the word are no pair, and the line reads as it does
        // on a page that draws its brackets mirrored.
        let logical = "1) \u{05D5} (\u{05E9}\u{05DC}\u{05D5}\u{05DD})";
        let display = logical_to_visual(logical, true);
        assert_eq!(logical_text(&display, true), logical);
        assert_eq!(logical_text_by_role(&display, true), logical);
    }

    #[test]
    fn an_inverted_pair_beside_a_latin_word_reads_as_a_mirrored_pair_does() {
        // `ABC )DEF( בא`: the pair stored by role encloses DEF, not ABC. The
        // line reads as `ABC (DEF) בא` does on a page that draws its pairs
        // mirrored; the screen cannot tell that from `(DEF) ABC` read
        // right to left, and takes the reading as one Latin phrase.
        let display = "ABC )DEF( \u{05D1}\u{05D0}";
        let mirrored = "ABC (DEF) \u{05D1}\u{05D0}";
        assert_eq!(
            logical_text_by_role(display, true),
            logical_text(mirrored, true)
        );
        assert_eq!(
            logical_text_by_role(display, true),
            "\u{05D0}\u{05D1} ABC (DEF)"
        );
    }

    #[test]
    fn adjacent_latin_pairs_do_not_vote_for_brackets_stored_by_role() {
        // `(ABC) א (DEF)`: the `)` closing one pair and the `(` opening the
        // next look like a backwards pair, but both pairs are drawn as
        // usual.
        assert!(!page_stores_by_role(&["(ABC) \u{05D0} (DEF)"]));
    }

    #[test]
    fn many_pairs_to_normalize_do_not_overflow_the_reading_masks() {
        // A role-stored line of adjacent `)1(` pairs followed by a Hebrew
        // letter: every pair left open by the display is an alternative,
        // far more than the masks try.
        for pairs in [4, 5, 33, 64, 70] {
            let display = ")1(".repeat(pairs) + "\u{05D0}";
            let logical = "\u{05D0}".to_string() + &"(1)".repeat(pairs);
            assert_eq!(logical_text_by_role(&display, true), logical);
        }
    }

    #[test]
    fn shown_items_are_not_read_as_stored_by_role() {
        // Two ActualText pairs, already as a display shows them, in front of
        // two pairs stored by role. Read as stored by role too, the `)` and
        // `(` of neighbouring pairs would pair up across the line.
        let items: Vec<(f32, &str)> =
            vec![(0.0, "(2)"), (10.0, "(2)"), (20.0, ")1("), (30.0, ")1(")];
        let read = |shown_items: &[bool]| {
            logical_line_order(
                &items,
                |item| item.1,
                |item| (item.0, 8.0),
                |_| 10.0,
                |_| true,
                None,
                LineDirection {
                    rtl_base: true,
                    brackets_by_role: true,
                    shown_items,
                },
            )
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
        };
        assert_eq!(
            read(&[true, true, false, false]),
            ["(1)", "(1)", "(2)", "(2)"]
        );
        // Unexempt, the same line comes out with inverted pairs.
        assert_ne!(read(&[]), ["(1)", "(1)", "(2)", "(2)"]);
    }

    #[test]
    fn a_numbered_pair_is_not_a_list_marker() {
        // Logical `1(שלום)` stored by role is `)םולש(1` on screen; the
        // number at the line's end belongs to the pair's `(`.
        let display = ")\u{05DD}\u{05D5}\u{05DC}\u{05E9}(1";
        assert_eq!(
            logical_text_by_role(display, true),
            "1(\u{05E9}\u{05DC}\u{05D5}\u{05DD})"
        );
        let heading = ")\u{05D0}(2";
        assert_eq!(logical_text_by_role(heading, true), "2(\u{05D0})");
        // Beside a pair of a digit, `1(א)` and `(1)` both stay pairs.
        let display = ")1(1)\u{05D0}(1";
        assert_eq!(logical_text_by_role(display, true), "1(\u{05D0})1(1)");
    }

    #[test]
    fn long_lines_of_unmatched_brackets_are_matched_in_linear_time() {
        let line: String = ")".repeat(50_000) + &"[".repeat(50_000) + " \u{05D0}";
        let started = std::time::Instant::now();
        assert!(!page_stores_by_role(&[line.as_str()]));
        assert!(started.elapsed().as_secs() < 3, "{:?}", started.elapsed());
    }

    #[test]
    fn a_page_stores_brackets_by_role_when_its_pairs_read_backwards() {
        // Pairs reading `)…(`, as on the Saudi PDPL pages.
        assert!(page_stores_by_role(&[")1( \u{05D1}\u{05D0}"]));
        // The `( b )` between two backwards pairs, and a parenthetical
        // wrapping onto the next line, are outnumbered.
        assert!(page_stores_by_role(&[
            ")\u{05D0}( \u{05D1} )1(",
            "\u{05D2} )\u{05D3}",
        ]));
        // A Latin pair reads `(…)` either way and says nothing.
        assert!(page_stores_by_role(&[
            ")1( \u{05D1}",
            "\u{05D1} (IASB) \u{05D0}",
        ]));
        // Pairs drawn mirrored outnumber a backwards one.
        assert!(!page_stores_by_role(&[
            ")1( \u{05D1}",
            "(\u{05D0}) \u{05D1} (2)",
        ]));
        // No brackets, no evidence.
        assert!(!page_stores_by_role(&["\u{05D0} \u{05D1}"]));
    }

    #[test]
    fn number_after_latin_word_stays_with_the_phrase() {
        // אב ABC 123 גד — "ABC 123" is one embedded phrase; the display
        // cannot tell it from "123 ABC", and the reading that keeps the
        // number after the word is taken.
        let logical = "\u{05D0}\u{05D1} ABC 123 \u{05D2}\u{05D3}";
        let display = logical_to_visual(logical, true);
        assert_eq!(display, "\u{05D3}\u{05D2} ABC 123 \u{05D1}\u{05D0}");
        assert_eq!(logical_text(&display, true), logical);
        // A date inside the RTL flow keeps its own order too.
        let logical = "\u{05D1}\u{05EA}\u{05D0}\u{05E8}\u{05D9}\u{05DA} 12/03/2024 \u{05D1}";
        assert_eq!(
            logical_text(&logical_to_visual(logical, true), true),
            logical
        );
    }

    #[test]
    fn a_dash_between_digits_stays_inside_the_number_after_arabic() {
        // Digits after an Arabic letter are Arabic numbers, which rule W4
        // would not join across a hyphen; a page range still reads whole.
        let logical = "\u{0627}لتعاريف 1-9";
        let display = logical_to_visual(logical, true);
        assert!(display.starts_with("1-9 "), "{display}");
        assert_eq!(logical_text(&display, true), logical);
        let logical = "\u{05D8}\u{05DC}. 03-1234567";
        assert_eq!(
            logical_text(&logical_to_visual(logical, true), true),
            logical
        );
    }

    #[test]
    fn combining_marks_stay_on_their_base() {
        // שָׁלוֹם with points: each mark follows its base in both orders.
        let logical = "\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD}";
        let display = logical_to_visual(logical, true);
        assert_eq!(
            display,
            "\u{05DD}\u{05D5}\u{05B9}\u{05DC}\u{05E9}\u{05C1}\u{05B8}"
        );
        assert_eq!(logical_text(&display, true), logical);
    }

    #[test]
    fn ltr_paragraph_with_an_embedded_rtl_word() {
        let logical =
            "see \u{05E9}\u{05DC}\u{05D5}\u{05DD} (\u{05E2}\u{05D5}\u{05DC}\u{05DD}) here";
        // The pair encloses RTL text after an RTL word, so it joins the RTL
        // run, which displays reversed as a whole.
        let display = logical_to_visual(logical, false);
        assert_eq!(
            display,
            "see (\u{05DD}\u{05DC}\u{05D5}\u{05E2}) \u{05DD}\u{05D5}\u{05DC}\u{05E9} here"
        );
        assert_eq!(logical_text(&display, false), logical);
    }

    #[test]
    fn arabic_with_arabic_indic_digits_and_percent() {
        // نسبة ٢٥٪ من الطلاب
        let logical = "\u{0646}\u{0633}\u{0628}\u{0629} \u{0662}\u{0665}\u{066A} \u{0645}\u{0646} \u{0627}\u{0644}\u{0637}\u{0644}\u{0627}\u{0628}";
        // Arabic-Indic digits keep their order; the Arabic percent sign is
        // not a European terminator and displays on the digits' left.
        let display = logical_to_visual(logical, true);
        assert!(display.contains("\u{066A}\u{0662}\u{0665}"), "{display}");
        assert_eq!(logical_text(&display, true), logical);
    }

    #[test]
    fn presentation_forms_normalize_to_letters_in_reading_order() {
        // Isolated lam-alef ligature then final noon, already in logical order.
        assert_eq!(
            normalize_presentation_forms("\u{FEFB}\u{FEE6}"),
            "\u{0644}\u{0627}\u{0646}"
        );
        // Hebrew presentation forms decompose to letter plus point.
        assert_eq!(normalize_presentation_forms("\u{FB2A}"), "\u{05E9}\u{05C1}");
        assert!(matches!(
            normalize_presentation_forms("plain"),
            Cow::Borrowed("plain")
        ));
        assert!(!is_presentation_form('\u{FEFF}'));
    }

    #[test]
    fn line_order_rebuilds_items_from_visual_text() {
        // Two words painted as visual-order runs left to right: item 0 is
        // the display of the LAST word, item 1 of the first.
        let items = [
            ("\u{05DD}\u{05DC}\u{05D5}\u{05E2}", 100.0f32, 30.0f32), // עולם reversed
            ("\u{05DD}\u{05D5}\u{05DC}\u{05E9}", 140.0, 30.0),       // שלום reversed
        ];
        let order = logical_line_order(
            &items,
            |i| i.0,
            |i| (i.1, i.2),
            |_| 12.0,
            |_| true,
            None,
            LineDirection {
                rtl_base: true,
                brackets_by_role: false,
                shown_items: &[],
            },
        );
        assert_eq!(
            order,
            vec![
                (1, "\u{05E9}\u{05DC}\u{05D5}\u{05DD}".to_string()),
                (0, "\u{05E2}\u{05D5}\u{05DC}\u{05DD}".to_string()),
            ]
        );
    }

    #[test]
    fn a_vowel_sign_shown_apart_follows_its_letter_in_the_reading() {
        // A pointed word one glyph per item in screen order, the sign an
        // item of its own over the second letter, inside its advance: the
        // sign follows that letter in the reading, not the letter read
        // before it, whether the items' texts are visual or logical.
        let items = [
            ("\u{05DC}", 110.0f32, 10.0f32),
            ("\u{05D0}", 120.0, 10.0),
            ("\u{05B8}", 124.0, 0.0),
            ("\u{05D1}", 130.0, 10.0),
        ];
        for visual in [false, true] {
            let order = logical_line_order(
                &items,
                |i| i.0,
                |i| (i.1, i.2),
                |_| 12.0,
                |_| visual,
                None,
                LineDirection {
                    rtl_base: true,
                    brackets_by_role: false,
                    shown_items: &[],
                },
            );
            assert_eq!(
                order,
                vec![
                    (3, "\u{05D1}".to_string()),
                    (1, "\u{05D0}".to_string()),
                    (2, "\u{05B8}".to_string()),
                    (0, "\u{05DC}".to_string()),
                ],
                "visual={visual}"
            );
        }
        // A sign shown before its letter, at the letter's own x, is not
        // over the letter to its left, and its neighbours place it as
        // before: after the letter read first of the two.
        let items = [
            ("\u{05DC}", 110.0f32, 10.0f32),
            ("\u{05B8}", 120.0, 0.0),
            ("\u{05D0}", 120.0, 10.0),
            ("\u{05D1}", 130.0, 10.0),
        ];
        let order = logical_line_order(
            &items,
            |i| i.0,
            |i| (i.1, i.2),
            |_| 12.0,
            |_| false,
            None,
            LineDirection {
                rtl_base: true,
                brackets_by_role: false,
                shown_items: &[],
            },
        );
        let reading: Vec<usize> = order.iter().map(|(index, _)| *index).collect();
        assert_eq!(reading, [3, 2, 1, 0]);
    }

    #[test]
    fn line_order_keeps_runs_whole_around_a_split_bracket_pair() {
        // "התקן (IFRS 16)" painted as three runs left to right: the display
        // glyphs "(IFRS", "16)" and the Hebrew word. Each run comes back
        // whole, in reading order.
        let items = [
            ("(IFRS", 100.0f32, 30.0f32),
            ("16)", 133.0, 16.0),
            ("\u{05DF}\u{05E7}\u{05EA}\u{05D4}", 160.0, 28.0),
        ];
        let order = logical_line_order(
            &items,
            |i| i.0,
            |i| (i.1, i.2),
            |_| 12.0,
            |_| true,
            None,
            LineDirection {
                rtl_base: true,
                brackets_by_role: false,
                shown_items: &[],
            },
        );
        assert_eq!(
            order,
            vec![
                (2, "\u{05D4}\u{05EA}\u{05E7}\u{05DF}".to_string()),
                (0, "(IFRS".to_string()),
                (1, "16)".to_string()),
            ]
        );
    }

    #[test]
    fn punctuation_runs_keep_their_page_neighbours() {
        // Two bracketed acronyms after a Hebrew phrase, the brackets in runs
        // of their own: "(LR1)(LR2)" displays as "(LR2)(LR1)". The inner
        // run ")(" holds one glyph of each pair; it stays between the two
        // acronyms and every run comes back whole.
        let items = [
            (" (", 462.0f32, 8.0f32),
            ("LR2", 470.4, 13.0),
            (")(", 483.3, 6.5),
            ("LR1", 489.8, 13.0),
            (")", 502.9, 3.3),
            (
                "\u{05E3}\u{05D5}\u{05E0}\u{05D9}\u{05DE}\u{05D4}",
                508.3,
                39.0,
            ),
        ];
        let order = logical_line_order(
            &items,
            |i| i.0,
            |i| (i.1, i.2),
            |_| 10.0,
            |_| true,
            None,
            LineDirection {
                rtl_base: true,
                brackets_by_role: false,
                shown_items: &[],
            },
        );
        assert_eq!(
            order,
            vec![
                (
                    5,
                    "\u{05D4}\u{05DE}\u{05D9}\u{05E0}\u{05D5}\u{05E3}".to_string()
                ),
                (4, "(".to_string()),
                (3, "LR1".to_string()),
                (2, ")(".to_string()),
                (1, "LR2".to_string()),
                (0, ") ".to_string()),
            ]
        );
    }

    #[test]
    fn line_order_keeps_logical_items_and_orders_embedded_ltr_forward() {
        // Logical-text items (a word per item) in screen order, as the line
        // "של IASB 42:" displays: the Latin phrase and its number read
        // forward, the RTL word comes first, the colon glyph at the far
        // left belongs after the number.
        let items = [
            (":", 100.0f32, 3.0f32),
            ("IASB", 103.5, 30.0),
            ("42", 140.0, 12.0),
            ("\u{05E9}\u{05DC}", 160.0, 20.0),
        ];
        let order = logical_line_order(
            &items,
            |i| i.0,
            |i| (i.1, i.2),
            |_| 12.0,
            |_| false,
            None,
            LineDirection {
                rtl_base: true,
                brackets_by_role: false,
                shown_items: &[],
            },
        );
        let indexes: Vec<usize> = order.iter().map(|(i, _)| *i).collect();
        assert_eq!(indexes, vec![3, 1, 2, 0]);
        assert_eq!(order[2].1, "42");
        assert_eq!(order[3].1, ":");
    }

    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[self.below(items.len())]
        }
        fn chance(&mut self, one_in: usize) -> bool {
            self.below(one_in) == 0
        }
    }

    const HEBREW_LETTERS: &[char] = &['א', 'ב', 'ג', 'ד', 'ה', 'ו', 'ל', 'מ', 'ש', 'ת', 'ם', 'ן'];
    const HEBREW_POINTS: &[char] = &['\u{05B8}', '\u{05B4}', '\u{05BC}', '\u{05C1}'];
    const ARABIC_LETTERS: &[char] = &['ا', 'ب', 'ت', 'ك', 'ل', 'م', 'ن', 'ع', 'س'];
    const ARABIC_MARKS: &[char] = &['\u{064E}', '\u{064F}', '\u{0651}'];
    const LATIN_LETTERS: &[char] = &['a', 'b', 'c', 'x', 'y', 'e', 'n', 'B', 'D', 'P'];
    const ARABIC_INDIC_DIGITS: &[char] = &['٠', '١', '٢', '٥', '٩'];

    /// A random word, number or punctuation token of the kind that makes
    /// up real lines: Hebrew or Arabic words (with the odd vowel point),
    /// Latin words, numbers in either digit system with their separators,
    /// percentages, dates, and punctuation attached to a word.
    fn token(rng: &mut XorShift) -> String {
        let word = |rng: &mut XorShift, letters: &[char], marks: &[char]| -> String {
            let mut w = String::new();
            for _ in 0..1 + rng.below(5) {
                w.push(*rng.pick(letters));
                if !marks.is_empty() && rng.chance(6) {
                    w.push(*rng.pick(marks));
                }
            }
            w
        };
        let number = |rng: &mut XorShift, digits: &[char]| -> String {
            let mut n: String = (0..1 + rng.below(4)).map(|_| *rng.pick(digits)).collect();
            match rng.below(6) {
                0 => {
                    n = format!(
                        "{n},{}{}{}",
                        rng.pick(digits),
                        rng.pick(digits),
                        rng.pick(digits)
                    )
                }
                1 => n = format!("{n}.{}", rng.pick(digits)),
                2 => n.push('%'),
                3 => n = format!("-{n}"),
                _ => {}
            }
            n
        };
        let mut t = match rng.below(10) {
            0..=3 => word(rng, HEBREW_LETTERS, HEBREW_POINTS),
            4 | 5 => word(rng, ARABIC_LETTERS, ARABIC_MARKS),
            6 | 7 => word(rng, LATIN_LETTERS, &[]),
            8 => number(rng, &['0', '1', '2', '5', '9']),
            _ => {
                if rng.chance(2) {
                    number(rng, ARABIC_INDIC_DIGITS)
                } else {
                    "12/03/2024".to_string()
                }
            }
        };
        if rng.chance(5) {
            t.push(*rng.pick(&[',', '.', ':', ';', '?', '!']));
        }
        t
    }

    /// A random line of one to seven tokens; one in four wraps a stretch of
    /// tokens in brackets or quotes.
    fn line(rng: &mut XorShift) -> String {
        let mut tokens: Vec<String> = (0..1 + rng.below(7)).map(|_| token(rng)).collect();
        if rng.chance(4) {
            let (open, close) = *rng.pick(&[('(', ')'), ('[', ']'), ('"', '"'), ('«', '»')]);
            let from = rng.below(tokens.len());
            let to = from + rng.below(tokens.len() - from);
            tokens[from].insert(0, open);
            tokens[to].push(close);
        }
        tokens.join(" ")
    }

    #[test]
    fn round_trip_recovers_random_lines() {
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        let mut exact = 0;
        let mut equivalent = 0;
        let mut failures: Vec<String> = Vec::new();
        for _ in 0..10_000 {
            let logical = line(&mut rng);
            for rtl_base in [true, false] {
                let display = logical_to_visual(&logical, rtl_base);
                let recovered = logical_text(&display, rtl_base);
                // Two logical lines can share one display line when a
                // neutral sits between runs whose order the display does
                // not pin down; the recovered line must then display the
                // same, which is all the page tells us.
                if recovered == logical {
                    exact += 1;
                } else if logical_to_visual(&recovered, rtl_base) == display {
                    equivalent += 1;
                } else {
                    failures.push(format!(
                        "rtl_base={rtl_base}: {logical:?} displayed as {display:?} came back as {recovered:?}"
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} lines came back displaying differently ({exact} exact, {equivalent} equivalent):\n{}",
            failures.len(),
            exact + equivalent + failures.len(),
            failures.iter().take(12).cloned().collect::<Vec<_>>().join("\n")
        );
        // The rest display the same as the line they came from: mostly a
        // number beside a Latin word, where the display does not say which
        // came first.
        assert!(
            exact * 10 >= 20_000 * 8,
            "round trip exact for {exact} of 20000 lines"
        );
    }

    /// Arbitrary character soup never panics, and the inversion is a
    /// permutation of the input (up to bracket mirroring).
    #[test]
    fn character_soup_is_permuted_without_panicking() {
        const SOUP: &[char] = &[
            'א', 'ب', '\u{05B8}', '\u{064E}', 'a', 'X', '1', '9', '٢', ' ', '(', ')', '[', ']',
            '.', ',', ':', '-', '%', '/', '"', '\n', '\t', '\u{200F}', '\u{202B}', '\u{2029}',
        ];
        let mut rng = XorShift(7);
        for _ in 0..3_000 {
            let text: Vec<char> = (0..rng.below(12)).map(|_| *rng.pick(SOUP)).collect();
            let visual: Vec<VisualChar> = text
                .iter()
                .enumerate()
                .map(|(i, &ch)| VisualChar { ch, item: Some(i) })
                .collect();
            for rtl_base in [true, false] {
                let logical = visual_to_logical(&visual, rtl_base, None);
                assert_eq!(logical.len(), text.len());
                let mut seen: Vec<usize> = logical.iter().filter_map(|v| v.item).collect();
                seen.sort_unstable();
                assert_eq!(seen, (0..text.len()).collect::<Vec<_>>());
                for v in &logical {
                    let original = text[v.item.unwrap()];
                    assert!(v.ch == original || v.ch == mirror_char(original));
                }
                let _ = logical_to_visual(&text.iter().collect::<String>(), rtl_base);
            }
        }
    }
}
