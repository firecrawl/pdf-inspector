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
            item_type: ItemType::Text,
            mcid,
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
