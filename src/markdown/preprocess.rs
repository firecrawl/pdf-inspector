//! Line preprocessing: heading merging, drop cap handling, and repeated line removal.

use std::collections::HashMap;

use crate::structure_tree::StructRole;
use crate::types::{TextItem, TextLine};

use super::analysis::detect_header_level;

/// Resolve a heading level for a line, considering both struct-tree roles and font heuristics.
/// Struct-tree headings take priority.
fn effective_heading_level(
    line: &TextLine,
    base_size: f32,
    heading_tiers: &[f32],
    struct_roles: Option<&HashMap<u32, HashMap<i64, StructRole>>>,
) -> Option<usize> {
    // Check struct-tree role first
    if let Some(roles) = struct_roles {
        if let Some(page_roles) = roles.get(&line.page) {
            for item in &line.items {
                if let Some(mcid) = item.mcid {
                    if let Some(role) = page_roles.get(&mcid) {
                        let level = match role {
                            StructRole::H => Some(1),
                            StructRole::H1 => Some(1),
                            StructRole::H2 => Some(2),
                            StructRole::H3 => Some(3),
                            StructRole::H4 => Some(4),
                            StructRole::H5 => Some(5),
                            StructRole::H6 => Some(6),
                            _ => None,
                        };
                        if level.is_some() {
                            return level;
                        }
                    }
                }
            }
        }
    }

    // Fall back to font-size heuristic
    let font = line.items.first().map(|i| i.font_size).unwrap_or(base_size);
    detect_header_level(
        font,
        base_size,
        heading_tiers,
        crate::markdown::analysis::line_is_mostly_bold(line),
    )
}

/// Merge consecutive heading lines at the same level into a single line.
///
/// When a heading wraps across multiple text lines (e.g., "About Glenair, the Mission-Critical"
/// and "Interconnect Company"), each fragment becomes a separate `# Header` in the output.
/// This function detects consecutive lines at the same heading tier on the same page
/// with a small Y gap and merges them into one line.
///
/// Both font-size heuristic headings and struct-tree tagged headings are considered.
pub(crate) fn merge_heading_lines(
    lines: Vec<TextLine>,
    base_size: f32,
    heading_tiers: &[f32],
    struct_roles: Option<&HashMap<u32, HashMap<i64, StructRole>>>,
) -> Vec<TextLine> {
    if lines.is_empty() {
        return lines;
    }

    let mut result: Vec<TextLine> = Vec::with_capacity(lines.len());

    for line in lines {
        let line_level = effective_heading_level(&line, base_size, heading_tiers, struct_roles);
        let line_font = line.items.first().map(|i| i.font_size).unwrap_or(base_size);

        // Check if the previous line is a heading at the same level on the same page
        let should_merge = if let (Some(prev), Some(curr_level)) = (result.last(), line_level) {
            let prev_level = effective_heading_level(prev, base_size, heading_tiers, struct_roles);
            let same_page = prev.page == line.page;
            let same_level = prev_level == Some(curr_level);
            let y_gap = prev.y - line.y;
            // Merge if gap is within ~2x the font size (normal line wrap spacing)
            let close_enough = y_gap > 0.0 && y_gap < line_font * 2.0;
            // Don't merge if combined text would be too long — real headings are short.
            // This prevents merging body-text lines that are mis-tagged as headings.
            let prev_words = prev.text().split_whitespace().count();
            let curr_words = line.text().split_whitespace().count();
            let not_too_long = prev_words + curr_words <= 20;
            same_page && same_level && close_enough && not_too_long
        } else {
            false
        };

        // Bold headings at body font size never reach a tier, so wrapped ones
        // split into two output headings ("…of wood pellets and cost" /
        // "structure in Japan"). Merge a fully-bold line into the previous
        // fully-bold line when it reads as a wrap continuation: starts
        // lowercase, tiny Y gap, and the previous line has no terminal
        // punctuation. Kept deliberately narrow — bold list labels and bold
        // sentences start with markers or capitals and are unaffected.
        let should_merge = should_merge
            || if let Some(prev) = result.last() {
                let all_bold = |l: &TextLine| {
                    !l.items.is_empty() && l.items.iter().all(|i: &TextItem| i.is_bold)
                };
                let prev_text = prev.text();
                let prev_trim = prev_text.trim_end();
                let curr_text = line.text();
                let curr_trim = curr_text.trim();
                let y_gap = prev.y - line.y;
                // Both lines must be tier-less: a tiered/tagged bold heading
                // followed by bold body text must not absorb it.
                line_level.is_none()
                    && effective_heading_level(prev, base_size, heading_tiers, struct_roles)
                        .is_none()
                    && prev.page == line.page
                    && all_bold(prev)
                    && all_bold(&line)
                    && y_gap > 0.0
                    && y_gap < line_font * 1.6
                    && curr_trim.chars().next().is_some_and(|c| c.is_lowercase())
                    && !prev_trim.ends_with(['.', ':', ';', '!', '?'])
                    && prev_trim.split_whitespace().count() + curr_trim.split_whitespace().count()
                        <= 20
            } else {
                false
            };

        if should_merge {
            // Append this line's items to the previous line
            let prev = result.last_mut().unwrap();
            // Add a space-bearing TextItem to separate the merged text
            if let Some(first_item) = line.items.first() {
                let mut space_item = first_item.clone();
                space_item.text = format!(" {}", space_item.text.trim_start());
                prev.items.push(space_item);
            }
            for item in line.items.into_iter().skip(1) {
                prev.items.push(item);
            }
        } else {
            result.push(line);
        }
    }

    result
}

/// Merge drop caps with the appropriate line.
/// A drop cap is a single large letter at the start of a paragraph.
/// Due to PDF coordinate sorting, the drop cap may appear AFTER the line it belongs to.
/// True when the text ends a sentence, as opposed to merely ending in a
/// period. An abbreviation or list marker ("e.g.", "Fig.", "Mr.", "1.")
/// closes with a period mid-sentence, so treating those as paragraph
/// boundaries would let a drop cap be prepended to a continuation.
fn ends_sentence(text: &str) -> bool {
    let t = text.trim_end();
    if t.ends_with(['!', '?']) {
        return true;
    }
    let Some(stripped) = t.strip_suffix('.') else {
        return false;
    };
    let last = stripped.split_whitespace().next_back().unwrap_or("");
    if last.is_empty() {
        return false;
    }

    // Abbreviations carry an internal period between very short segments
    // ("e.g.", "i.e.", "U.S."). Domains and decimals have the same shape but
    // longer or numeric segments ("example.com.", "3.14."), and those end
    // sentences perfectly well, so require every segment to be short and
    // alphabetic before reading the internal period as an abbreviation.
    if last.contains('.')
        && last
            .split('.')
            .filter(|seg| !seg.is_empty())
            // Characters, not bytes: a two-letter non-ASCII abbreviation
            // ("т.е.", "ú.d.") measures four or more bytes and would
            // otherwise be read as a completed sentence.
            .all(|seg| seg.chars().count() <= 2 && seg.chars().all(char::is_alphabetic))
    {
        return false;
    }

    // Enumerators stand alone on their line ("1.", "ii.", "IV."). A number
    // or numeral in the tail of a sentence does not — "published in 2020.",
    // "He scored 5." and "after World War II." all end sentences, and
    // treating them as markers would block a legitimate drop-cap merge.
    if stripped.split_whitespace().count() == 1 {
        let is_numeric = last.chars().all(|c| c.is_ascii_digit());
        let is_roman = last
            .chars()
            .all(|c| matches!(c.to_ascii_uppercase(), 'I' | 'V' | 'X' | 'L' | 'C'));
        if is_numeric || is_roman {
            return false;
        }
    }

    const ABBREVIATIONS: &[&str] = &[
        "Fig", "No", "Mr", "Mrs", "Ms", "Dr", "St", "vs", "etc", "al", "Ed", "Eq", "Ch", "pp",
        "Vol", "cf", "Prof", "Inc", "Ltd", "Jr", "Sr",
    ];
    !ABBREVIATIONS.iter().any(|a| a.eq_ignore_ascii_case(last))
}

pub(crate) fn merge_drop_caps(lines: Vec<TextLine>, base_size: f32) -> Vec<TextLine> {
    let mut result: Vec<TextLine> = Vec::with_capacity(lines.len());

    for line in &lines {
        let text = line.text();
        let trimmed = text.trim();

        // Check if this looks like a drop cap:
        // 1. Single character (or single char + space)
        // 2. Much larger than base font (3x or more)
        // 3. The character is uppercase
        let is_drop_cap = trimmed.len() <= 2
            && line.items.first().map(|i| i.font_size).unwrap_or(0.0) >= base_size * 2.5
            && trimmed
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false);

        // Embedded drop cap: a two-line cap's baseline aligns with the
        // paragraph's SECOND line, so Y-grouping puts the glyph at the start
        // of that line rather than on a line of its own. Left there it
        // surfaces mid-sentence once the paragraph is joined — Shannon's
        // "A Mathematical Theory of Communication" reads "...which exchange
        // T bandwidth for signal-to-noise ratio...". Detect it, prepend the
        // character to the paragraph's first line, and drop it from this one.
        //
        // The size gate is 1.8x rather than 2.5x because bitmap (Type3) caps
        // report their glyph bbox rather than the em box, so a two-line cap
        // can measure as little as ~1.9x the body size.
        if line.items.len() > 1 {
            let first = &line.items[0];
            // The remainder must be a substantive body run: a lone label or
            // math fragment beside a large glyph is not a drop-cap paragraph.
            let rest_letters: usize = line.items[1..]
                .iter()
                .map(|i| i.text.chars().filter(|c| c.is_alphabetic()).count())
                .sum();
            let is_embedded_cap = first.font_size >= base_size * 1.8
                && first.text.trim().chars().count() == 1
                && first
                    .text
                    .trim()
                    .chars()
                    .next()
                    .is_some_and(char::is_uppercase)
                && line.items[1..]
                    .iter()
                    .all(|i| i.font_size < base_size * 1.5)
                && line.items[1..].iter().any(|i| i.x > first.x)
                && rest_letters >= 8;
            if is_embedded_cap {
                let drop_char = first.text.trim().chars().next().unwrap();
                let cap_x = first.x;
                let line_y = line.y;
                // Text on the cap's own line, pushed right to clear the glyph.
                let rest_x = line.items[1].x;

                // Walk up the run of lines the cap has indented. A drop cap
                // pushes every line it covers to the right of the glyph, so
                // the paragraph's first line is the TOPMOST line sharing that
                // indent — however many lines the cap spans. Using the indent
                // rather than the cap's font size is what makes this work for
                // three- and four-line initials as well as two-line ones;
                // deriving a line count from the em size does not survive
                // contact with real documents, where 36-47pt initials sit
                // over 11-14pt leading.
                //
                // A cap in a different column has no such run (its neighbours
                // sit at an unrelated x), so it is left alone — which is
                // correct when the cap's own line already carries the rest of
                // the word.
                const INDENT_TOLERANCE: f32 = 2.0;
                const MAX_CAP_LINES: usize = 8;
                let max_step = base_size * 2.5;
                let mut target_idx = result.len();
                let mut expected_y = line_y;
                while target_idx > 0 && result.len() - target_idx < MAX_CAP_LINES {
                    let cand = &result[target_idx - 1];
                    let step = cand.y - expected_y;
                    let shares_indent = cand
                        .items
                        .first()
                        .is_some_and(|i| (i.x - rest_x).abs() <= INDENT_TOLERANCE);
                    if cand.page != line.page || step <= 0.0 || step > max_step || !shares_indent {
                        break;
                    }
                    expected_y = cand.y;
                    target_idx -= 1;
                }

                // The topmost line of the run is the paragraph's first line.
                // The line above THAT tells us whether it starts a paragraph.
                let before_target = target_idx
                    .checked_sub(1)
                    .and_then(|i| result.get(i))
                    .filter(|l| l.page == line.page)
                    .map(|l| (l.text().trim_end().to_string(), l.y));
                // Leading within the run: the step from the target down to the
                // next line of the paragraph, which is the cap's own line when
                // the run is a single line.
                let run_step = result
                    .get(target_idx)
                    .map(|t| {
                        let below_y = result.get(target_idx + 1).map_or(line_y, |b| b.y);
                        t.y - below_y
                    })
                    .unwrap_or(0.0);
                let step_for_gap = if run_step > 0.0 {
                    run_step
                } else {
                    base_size * 1.2
                };

                let target = (target_idx < result.len())
                    .then(|| &mut result[target_idx])
                    .filter(|prev| {
                        let prev_text = prev.text();
                        let prev_trimmed = prev_text.trim();
                        // A hyphen on the line above means the target resumes
                        // a split word, so it continues a paragraph rather
                        // than starting one (polkuja_ylakoulu: "ylakou-" +
                        // "lulaisten").
                        //
                        // Case cannot serve as a continuation signal here: the
                        // target legitimately starts lowercase, because the
                        // cap removes the word's first letter and leaves
                        // "ver the course..." for "Over".
                        let continues_previous = before_target
                            .as_ref()
                            .is_some_and(|(b, _)| b.ends_with('-'));
                        // The target must START a paragraph: extra leading
                        // above it, a completed sentence on the line above, or
                        // nothing above it at all.
                        let starts_paragraph = match before_target.as_ref() {
                            None => true,
                            Some((text, y)) => {
                                y - prev.y > step_for_gap * 1.15 || ends_sentence(text)
                            }
                        };
                        !continues_previous
                            && starts_paragraph
                            && prev.page == line.page
                            && prev.y > line_y
                            // Indented past the cap glyph, not merely to its
                            // right by an arbitrary amount.
                            && prev
                                .items
                                .first()
                                .is_some_and(|i| i.x > cap_x && i.x - cap_x <= first.font_size * 2.0)
                            // Body text, so headings, labels and table
                            // fragments are never rewritten.
                            && prev_trimmed
                                .chars()
                                .next()
                                .is_some_and(char::is_alphabetic)
                            && prev_trimmed.chars().filter(|c| c.is_alphabetic()).count() >= 8
                    });
                if let Some(prev_line) = target {
                    if let Some(first_item) = prev_line.items.first_mut() {
                        // A mid-word cap ("T" + "HE recent") joins directly.
                        // Leading whitespace only marks a word boundary when
                        // the cap is itself a single-letter word, since the
                        // paragraph's indent can also arrive as whitespace.
                        const SINGLE_LETTER_WORDS: &[char] = &['A', 'I', 'O', 'U', 'Y', 'E'];
                        let had_leading_ws = first_item.text.starts_with(char::is_whitespace)
                            && SINGLE_LETTER_WORDS.contains(&drop_char);
                        let rest = first_item.text.trim_start().to_string();
                        first_item.text = if had_leading_ws {
                            format!("{} {}", drop_char, rest)
                        } else {
                            format!("{}{}", drop_char, rest)
                        };
                    }
                    let mut line = line.clone();
                    line.items.remove(0);
                    result.push(line);
                    continue;
                }
            }
        }

        if is_drop_cap {
            let drop_char = trimmed.chars().next().unwrap();

            // Find the first line that starts with lowercase and is at the START of a paragraph
            // (i.e., preceded by a header or non-lowercase-starting line)
            let mut target_idx: Option<usize> = None;

            for (idx, prev_line) in result.iter().enumerate() {
                if prev_line.page != line.page {
                    continue;
                }

                let prev_text = prev_line.text();
                let prev_trimmed = prev_text.trim();

                // Check if this line starts with lowercase
                if prev_trimmed
                    .chars()
                    .next()
                    .map(|c| c.is_lowercase())
                    .unwrap_or(false)
                {
                    // Check if previous line exists and doesn't start with lowercase
                    // (meaning this is the start of a paragraph)
                    let is_para_start = if idx == 0 {
                        true
                    } else {
                        let before = result[idx - 1].text();
                        let before_trimmed = before.trim();
                        !before_trimmed
                            .chars()
                            .next()
                            .map(|c| c.is_lowercase())
                            .unwrap_or(true)
                    };

                    if is_para_start {
                        target_idx = Some(idx);
                        break;
                    }
                }
            }

            // Merge with the target line
            if let Some(idx) = target_idx {
                if let Some(first_item) = result[idx].items.first_mut() {
                    let prev_text = first_item.text.trim().to_string();
                    first_item.text = format!("{}{}", drop_char, prev_text);
                }
            }
            // Don't add the drop cap line itself
            continue;
        }

        result.push(line.clone());
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ItemType, TextItem};

    fn make_item(text: &str, font_size: f32, mcid: Option<i64>) -> TextItem {
        TextItem {
            text: text.to_string(),
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: font_size,
            font: "TestFont".to_string(),
            font_tag: String::new(),
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            rotation: 0.0,
            advance_known: true,
            item_type: ItemType::Text,
            mcid,
            baseline_shift: 0.0,
        }
    }

    fn make_line(text: &str, font_size: f32, page: u32, y: f32, mcid: Option<i64>) -> TextLine {
        TextLine {
            items: vec![make_item(text, font_size, mcid)],
            y,
            page,
            adaptive_threshold: 0.10,
        }
    }

    fn make_item_at(text: &str, font_size: f32, x: f32) -> TextItem {
        let mut item = make_item(text, font_size, None);
        item.x = x;
        item.width = text.len() as f32 * font_size * 0.5;
        item
    }

    #[test]
    fn embedded_drop_cap_moves_to_paragraph_start() {
        // A two-line cap baseline-aligns with the paragraph's SECOND line,
        // so it lands as that line's first item (Shannon entropy.pdf p.1).
        let first_line = TextLine {
            items: vec![make_item_at(
                "HE recent development which exchange",
                10.0,
                90.0,
            )],
            // 16pt baseline step under a 25pt cap: a genuine two-line cap.
            y: 716.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let second_line = TextLine {
            items: vec![
                make_item_at("T", 25.0, 72.0),
                make_item_at("bandwidth for signal-to-noise ratio", 10.0, 90.0),
            ],
            y: 700.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let result = merge_drop_caps(vec![first_line, second_line], 10.0);
        assert_eq!(result.len(), 2);
        assert!(
            result[0].text().starts_with("THE recent"),
            "cap should prepend to the paragraph start: {}",
            result[0].text()
        );
        assert!(
            result[1].text().starts_with("bandwidth"),
            "cap must be removed from the second line: {}",
            result[1].text()
        );
    }

    #[test]
    fn embedded_drop_cap_walks_a_multi_line_initial_to_the_paragraph_start() {
        // A 47pt initial over 13pt leading covers four lines, so the
        // paragraph's first line is three lines above the cap rather than
        // immediately above it (polkuja_ylakoulu). The indented run, not the
        // cap's em size, is what locates it.
        let mut lines = vec![TextLine {
            items: vec![make_item_at("Previous paragraph ends here.", 10.0, 72.0)],
            y: 766.0,
            page: 1,
            adaptive_threshold: 0.10,
        }];
        for (i, text) in [
            "rilaiset mediasisallot ovat tarkea osa",
            "useimpien ylakoululaisten elamaa ja",
            "muuta tekstia jatkuu tassa viela",
        ]
        .iter()
        .enumerate()
        {
            lines.push(TextLine {
                items: vec![make_item_at(text, 10.0, 90.0)],
                y: 753.0 - 13.0 * i as f32,
                page: 1,
                adaptive_threshold: 0.10,
            });
        }
        lines.push(TextLine {
            items: vec![
                make_item_at("E", 47.0, 72.0),
                make_item_at("loppuosa tekstista tassa", 10.0, 90.0),
            ],
            y: 714.0,
            page: 1,
            adaptive_threshold: 0.10,
        });

        let result = merge_drop_caps(lines, 10.0);
        assert!(
            result[1].text().starts_with("Erilaiset"),
            "cap belongs on the topmost line of the indented run: {}",
            result[1].text()
        );
        assert!(
            result[2].text().starts_with("useimpien"),
            "intervening run lines must be untouched: {}",
            result[2].text()
        );
        assert!(
            result[4].text().starts_with("loppuosa"),
            "cap must be removed from its own line: {}",
            result[4].text()
        );
    }

    #[test]
    fn embedded_drop_cap_ignores_non_paragraph_neighbours() {
        // Same geometry, but the preceding line is a short label rather than
        // body text, so it must not be rewritten.
        let label = TextLine {
            items: vec![make_item_at("Fig. 2", 10.0, 90.0)],
            y: 716.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let second_line = TextLine {
            items: vec![
                make_item_at("T", 25.0, 72.0),
                make_item_at("bandwidth for signal-to-noise ratio", 10.0, 90.0),
            ],
            y: 700.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let result = merge_drop_caps(vec![label, second_line], 10.0);
        assert_eq!(result[0].text().trim(), "Fig. 2");
        assert!(
            result[1].text().starts_with('T'),
            "cap stays put: {}",
            result[1].text()
        );
    }

    #[test]
    fn embedded_drop_cap_keeps_a_space_for_standalone_word_caps() {
        // Leading whitespace on the paragraph's first item marks the cap as
        // a word of its own rather than the first letter of one.
        let mut lead = make_item_at("long time ago in a galaxy far away", 10.0, 90.0);
        lead.text = " long time ago in a galaxy far away".to_string();
        let first_line = TextLine {
            items: vec![lead],
            y: 716.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let second_line = TextLine {
            items: vec![
                make_item_at("A", 25.0, 72.0),
                make_item_at("continued here with more body text", 10.0, 90.0),
            ],
            y: 700.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let result = merge_drop_caps(vec![first_line, second_line], 10.0);
        assert!(
            result[0].text().starts_with("A long time ago"),
            "standalone-word cap keeps one space: {}",
            result[0].text()
        );
    }

    #[test]
    fn embedded_drop_cap_skips_hyphenation_continuation_targets() {
        // The line above the RUN ends on a hyphen, so the run's topmost line
        // resumes a split word rather than starting a paragraph. It sits at
        // the paragraph margin (x=72), outside the cap's indent, so it is not
        // part of the run itself.
        let split_word = TextLine {
            items: vec![make_item_at(
                "mediasisallot ovat osa useimpien ylakou-",
                10.0,
                72.0,
            )],
            y: 728.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let run_top = TextLine {
            items: vec![make_item_at(
                "lulaisten elamaa ja muuta tekstia",
                10.0,
                90.0,
            )],
            y: 714.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let cap_line = TextLine {
            items: vec![
                make_item_at("E", 25.0, 72.0),
                make_item_at("jatkuu tassa lisaa leipatekstia", 10.0, 90.0),
            ],
            y: 700.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let result = merge_drop_caps(vec![split_word, run_top, cap_line], 10.0);
        assert!(
            result[1].text().starts_with("lulaisten"),
            "a run resuming a split word must not receive the cap: {}",
            result[1].text()
        );
        assert!(
            result[2].text().starts_with('E'),
            "cap stays put when no valid target exists: {}",
            result[2].text()
        );
    }

    #[test]
    fn embedded_drop_cap_indent_is_not_a_word_boundary() {
        // The paragraph's first line is indented to clear the cap, and that
        // indent can arrive as leading whitespace. A mid-word cap must still
        // join directly — "T HE recent" would be the defect this fixes.
        let mut lead = make_item_at("HE recent development and more body text", 10.0, 90.0);
        lead.text = "  HE recent development and more body text".to_string();
        let first_line = TextLine {
            items: vec![lead],
            y: 716.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let cap_line = TextLine {
            items: vec![
                make_item_at("T", 25.0, 72.0),
                make_item_at("bandwidth for signal-to-noise ratio", 10.0, 90.0),
            ],
            y: 700.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let result = merge_drop_caps(vec![first_line, cap_line], 10.0);
        assert!(
            result[0].text().starts_with("THE recent"),
            "indent must not be read as a word boundary: {}",
            result[0].text()
        );
    }

    #[test]
    fn ends_sentence_rejects_abbreviations_and_markers() {
        use super::ends_sentence;
        assert!(ends_sentence("This completes the thought."));
        assert!(ends_sentence("Is that so?"));
        assert!(ends_sentence("Stop!"));
        // Periods that do not end a sentence.
        assert!(!ends_sentence("as shown in Fig."));
        assert!(!ends_sentence("see e.g."));
        // Non-ASCII abbreviations. The two-CHARACTER segment is the case
        // that distinguishes a character count from a byte count: "пр" is
        // 2 chars but 4 bytes, so a byte-based bound would reject it and
        // read the line as a completed sentence.
        assert!(!ends_sentence("и т.пр."));
        assert!(!ends_sentence("см. т.е."));
        assert!(!ends_sentence("napr. ú.d."));
        assert!(!ends_sentence("reviewed by Dr."));
        // Standalone enumerators, any case.
        assert!(!ends_sentence("1."));
        assert!(!ends_sentence("IV."));
        assert!(!ends_sentence("ii."));
        assert!(!ends_sentence("xii."));
        // Numbers and numerals that genuinely end a sentence must count,
        // or a legitimate drop-cap merge is blocked.
        assert!(ends_sentence("The paper was published in 2020."));
        assert!(ends_sentence("He scored 5."));
        assert!(ends_sentence("after World War II."));
        assert!(ends_sentence("the constant equals 3.14."));
        assert!(ends_sentence("documented at example.com."));
        assert!(!ends_sentence("a trailing clause with no period"));
    }

    #[test]
    fn embedded_drop_cap_allows_first_paragraph_on_a_new_page() {
        // The line two back is on the previous page, so its y is unrelated
        // and must not be used as leading evidence.
        let prev_page_tail = TextLine {
            items: vec![make_item_at(
                "tail of the previous page body text",
                10.0,
                90.0,
            )],
            y: 90.0,
            page: 1,
            adaptive_threshold: 0.10,
        };
        let first_line = TextLine {
            items: vec![make_item_at(
                "HE recent development which exchange",
                10.0,
                90.0,
            )],
            y: 716.0,
            page: 2,
            adaptive_threshold: 0.10,
        };
        let cap_line = TextLine {
            items: vec![
                make_item_at("T", 25.0, 72.0),
                make_item_at("bandwidth for signal-to-noise ratio", 10.0, 90.0),
            ],
            y: 700.0,
            page: 2,
            adaptive_threshold: 0.10,
        };
        let result = merge_drop_caps(vec![prev_page_tail, first_line, cap_line], 10.0);
        assert!(
            result[1].text().starts_with("THE recent"),
            "a page break must not suppress the merge: {}",
            result[1].text()
        );
    }

    #[test]
    fn test_merge_struct_tree_headings() {
        // Two consecutive lines tagged as H2 via struct tree, same font size as body
        let lines = vec![
            make_line(
                "Historical Context for Operations in Snow",
                12.0,
                1,
                700.0,
                Some(10),
            ),
            make_line("Lake District", 12.0, 1, 686.0, Some(11)),
            make_line("Body text paragraph.", 12.0, 1, 660.0, Some(12)),
        ];

        let mut page_roles = HashMap::new();
        let mut roles = HashMap::new();
        roles.insert(10i64, StructRole::H2);
        roles.insert(11i64, StructRole::H2);
        roles.insert(12i64, StructRole::P);
        page_roles.insert(1u32, roles);

        let result = merge_heading_lines(lines, 12.0, &[], Some(&page_roles));
        assert_eq!(result.len(), 2, "should merge two H2 lines into one");
        let merged_text = result[0].text();
        assert!(
            merged_text.contains("Snow") && merged_text.contains("Lake"),
            "merged heading should contain both fragments: {merged_text}"
        );
    }

    #[test]
    fn test_no_merge_different_struct_levels() {
        // Two consecutive lines tagged as different heading levels
        let lines = vec![
            make_line("Chapter 1", 12.0, 1, 700.0, Some(10)),
            make_line("Introduction", 12.0, 1, 686.0, Some(11)),
        ];

        let mut page_roles = HashMap::new();
        let mut roles = HashMap::new();
        roles.insert(10i64, StructRole::H1);
        roles.insert(11i64, StructRole::H2);
        page_roles.insert(1u32, roles);

        let result = merge_heading_lines(lines, 12.0, &[], Some(&page_roles));
        assert_eq!(result.len(), 2, "should not merge different heading levels");
    }

    #[test]
    fn test_no_merge_heading_with_body() {
        // A heading line followed by a body paragraph line
        let lines = vec![
            make_line("Introduction", 12.0, 1, 700.0, Some(10)),
            make_line("This is body text.", 12.0, 1, 686.0, Some(11)),
        ];

        let mut page_roles = HashMap::new();
        let mut roles = HashMap::new();
        roles.insert(10i64, StructRole::H1);
        roles.insert(11i64, StructRole::P);
        page_roles.insert(1u32, roles);

        let result = merge_heading_lines(lines, 12.0, &[], Some(&page_roles));
        assert_eq!(result.len(), 2, "should not merge heading with body text");
    }

    #[test]
    fn test_merge_font_headings_still_works() {
        // Original font-size based merging should still work without struct roles
        let lines = vec![
            make_line("A Very Long Heading That", 18.0, 1, 700.0, None),
            make_line("Wraps to Next Line", 18.0, 1, 682.0, None),
            make_line("Body text.", 12.0, 1, 660.0, None),
        ];

        let heading_tiers = vec![18.0];
        let result = merge_heading_lines(lines, 12.0, &heading_tiers, None);
        assert_eq!(result.len(), 2, "should merge font-based heading lines");
    }

    fn make_bold_line(text: &str, page: u32, y: f32) -> TextLine {
        let mut item = make_item(text, 12.0, None);
        item.is_bold = true;
        TextLine {
            items: vec![item],
            y,
            page,
            adaptive_threshold: 0.10,
        }
    }

    #[test]
    fn merge_wrapped_bold_heading_lowercase_continuation() {
        // Bold-at-body-size heading wrapped across two lines: the second line
        // starts lowercase and must merge into the first.
        let lines = vec![
            make_bold_line(
                "3. Perspective of supply and demand balance and cost",
                1,
                700.0,
            ),
            make_bold_line("structure in Japan", 1, 686.0),
            make_line("Body text paragraph follows here.", 12.0, 1, 660.0, None),
        ];
        let result = merge_heading_lines(lines, 12.0, &[], None);
        assert_eq!(result.len(), 2, "wrapped bold heading should merge");
        assert!(result[0].text().contains("cost structure in Japan"));
    }

    #[test]
    fn no_merge_for_bold_sentences_or_new_headings() {
        // Second bold line starts with a capital — a new heading or label,
        // not a wrap continuation.
        let lines = vec![
            make_bold_line("Replace", 1, 700.0),
            make_bold_line("Trash", 1, 686.0),
        ];
        let result = merge_heading_lines(lines, 12.0, &[], None);
        assert_eq!(result.len(), 2, "distinct bold lines must not merge");

        // Previous line ends a sentence — continuation must not merge.
        let lines = vec![
            make_bold_line("This is a bold sentence.", 1, 700.0),
            make_bold_line("another bold line", 1, 686.0),
        ];
        let result = merge_heading_lines(lines, 12.0, &[], None);
        assert_eq!(result.len(), 2, "sentence-final bold line must not merge");
    }

    #[test]
    fn tiered_bold_heading_does_not_absorb_bold_body() {
        // Previous line is a tier-level bold heading (16pt vs 12pt body);
        // a following lowercase bold body line must NOT merge into it.
        let mut heading = make_bold_line("Section Title", 1, 700.0);
        heading.items[0].font_size = 16.0;
        heading.items[0].height = 16.0;
        let lines = vec![
            heading,
            make_bold_line("emphasized body text continues here", 1, 686.0),
        ];
        let result = merge_heading_lines(lines, 12.0, &[16.0], None);
        assert_eq!(result.len(), 2, "tiered heading must not absorb bold body");
    }
}
