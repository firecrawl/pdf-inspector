//! Font statistics, heading detection, and document structure analysis.

use std::collections::HashMap;

use crate::types::{TextItem, TextLine};
use log::debug;

/// Font statistics for a document
pub(crate) struct FontStats {
    pub(crate) most_common_size: f32,
}

/// Calculate font stats directly from items (before grouping into lines)
pub(crate) fn calculate_font_stats_from_items(items: &[TextItem]) -> FontStats {
    let mut size_counts: HashMap<i32, usize> = HashMap::new();

    for item in items {
        if item.font_size >= 9.0 {
            let size_key = (item.font_size * 10.0) as i32;
            *size_counts.entry(size_key).or_insert(0) += 1;
        }
    }

    // Break ties by preferring the smaller font size for deterministic output
    let most_common_size = size_counts
        .iter()
        .max_by(|(size_a, count_a), (size_b, count_b)| {
            count_a.cmp(count_b).then_with(|| size_b.cmp(size_a))
        })
        .map(|(size, _)| *size as f32 / 10.0)
        .unwrap_or(12.0);

    FontStats { most_common_size }
}

/// Calculate font stats from grouped lines
pub(crate) fn calculate_font_stats(lines: &[TextLine]) -> FontStats {
    let mut size_counts: HashMap<i32, usize> = HashMap::new();

    for line in lines {
        // Count once per line (first item) to give each line equal weight
        // Prevents small captions/footnotes from skewing the base
        if let Some(first) = line.items.first() {
            if first.font_size >= 9.0 {
                let size_key = (first.font_size * 10.0) as i32;
                *size_counts.entry(size_key).or_insert(0) += 1;
            }
        }
    }

    // Break ties by preferring the smaller font size for deterministic output
    let most_common_size = size_counts
        .iter()
        .max_by(|(size_a, count_a), (size_b, count_b)| {
            count_a.cmp(count_b).then_with(|| size_b.cmp(size_a))
        })
        .map(|(size, _)| *size as f32 / 10.0)
        .unwrap_or(12.0);

    FontStats { most_common_size }
}

/// Detect TOC-style lines that contain dot leaders (e.g., "Section Name .... 42").
/// These lines should never be joined with adjacent lines into a paragraph.
/// Handles both consecutive dots ("....") and spaced dots ("...   ...").
pub(crate) fn has_dot_leaders(text: &str) -> bool {
    // Consecutive dots (4+)
    if text.contains("....") {
        return true;
    }
    // Spaced dot leaders: "..." followed by whitespace and more dots
    // Count occurrences of "..." (3+ dots) — if 2+ groups, it's a dot leader
    let mut dot_groups = 0;
    let mut consecutive_dots = 0;
    for ch in text.chars() {
        if ch == '.' {
            consecutive_dots += 1;
        } else {
            if consecutive_dots >= 3 {
                dot_groups += 1;
            }
            consecutive_dots = 0;
        }
    }
    if consecutive_dots >= 3 {
        dot_groups += 1;
    }
    dot_groups >= 2
}

/// Compute the Y-gap threshold for paragraph break detection.
///
/// Instead of using a fixed multiple of base_size (which fails for double-spaced
/// documents), we compute the document's typical (median) line spacing and use
/// a multiplier on that. A gap significantly larger than typical indicates a
/// paragraph break.
///
/// Fallback: if we can't compute typical spacing, use base_size * 1.8.
pub(crate) fn compute_paragraph_threshold(lines: &[TextLine], base_size: f32) -> f32 {
    let fallback = base_size * 1.8;

    // Collect Y gaps between consecutive lines on the same page
    let mut gaps: Vec<f32> = Vec::new();
    let mut prev_y: Option<(u32, f32)> = None;

    for line in lines {
        if let Some((prev_page, py)) = prev_y {
            if line.page == prev_page {
                let gap = py - line.y;
                // Only consider positive gaps within a reasonable range
                // (skip huge gaps from page headers/footers)
                if gap > 0.0 && gap < base_size * 10.0 {
                    gaps.push(gap);
                }
            }
        }
        prev_y = Some((line.page, line.y));
    }

    if gaps.len() < 5 {
        return fallback;
    }

    gaps.sort_by(|a, b| a.total_cmp(b));

    let median = gaps[gaps.len() / 2];

    let threshold = (median * 1.3).max(base_size * 1.5);

    debug!(
        "paragraph_threshold: base_size={:.1} median_gap={:.1} threshold={:.1} ({} gaps sampled)",
        base_size,
        median,
        threshold,
        gaps.len()
    );

    if log::log_enabled!(log::Level::Debug) {
        // Gap histogram
        let buckets: &[f32] = &[0.0, 0.5, 1.0, 1.2, 1.5, 1.8, 2.0, 2.5, 3.0, 5.0, 10.0];
        for i in 0..buckets.len() - 1 {
            let count = gaps
                .iter()
                .filter(|&&g| {
                    let r = g / base_size;
                    r >= buckets[i] && r < buckets[i + 1]
                })
                .count();
            if count > 0 {
                debug!(
                    "  gap_ratio {:.1}-{:.1}: {}",
                    buckets[i],
                    buckets[i + 1],
                    count
                );
            }
        }
        let over = gaps.iter().filter(|&&g| g / base_size >= 10.0).count();
        if over > 0 {
            debug!("  gap_ratio 10.0+: {}", over);
        }
    }

    // Per-line detail: Y position, gap, ratio, bold, text preview, paragraph marker
    if log::log_enabled!(log::Level::Trace) {
        let mut prev: Option<(u32, f32)> = None;
        for line in lines {
            let font_size = line.items.first().map(|i| i.font_size).unwrap_or(0.0);
            let is_bold = line.items.first().map(|i| i.is_bold).unwrap_or(false);
            let text = line.text();
            let display: String = text.chars().take(80).collect();

            let (gap_str, ratio_str, marker) = if let Some((pp, py)) = prev {
                if pp == line.page {
                    let gap = py - line.y;
                    let ratio = gap / base_size;
                    let is_para = gap > threshold;
                    (
                        format!("{:8.1}", gap),
                        format!("{:8.2}", ratio),
                        if is_para { " <<PARA>>" } else { "" },
                    )
                } else {
                    ("     ---".to_string(), "     ---".to_string(), "")
                }
            } else {
                ("     ---".to_string(), "     ---".to_string(), "")
            };

            log::trace!(
                "  p={} y={:8.1} gap={} ratio={} fs={:5.1} {}  {}{}",
                line.page,
                line.y,
                gap_str,
                ratio_str,
                font_size,
                if is_bold { "B" } else { " " },
                display,
                marker
            );

            prev = Some((line.page, line.y));
        }
    }

    threshold
}

/// Discover distinct heading font-size tiers in the document.
/// Returns tiers sorted largest-first (tier 0 = H1, tier 1 = H2, …).
/// Sizes within 0.5pt are clustered into the same tier. Capped at 4 tiers.
pub(crate) fn compute_heading_tiers(lines: &[TextLine], base_size: f32) -> Vec<f32> {
    let mut heading_sizes: Vec<f32> = Vec::new();

    for line in lines {
        if let Some(first) = line.items.first() {
            if first.font_size / base_size >= 1.2 {
                heading_sizes.push(first.font_size);
            }
        }
    }

    // Sort descending
    heading_sizes.sort_by(|a, b| b.total_cmp(a));

    // Cluster sizes within 0.5pt into same tier (use first value as representative)
    let mut tiers: Vec<f32> = Vec::new();
    for size in heading_sizes {
        let already_in_tier = tiers.iter().any(|&t| (t - size).abs() < 0.5);
        if !already_in_tier {
            tiers.push(size);
        }
    }

    // Cap at 4 tiers
    tiers.truncate(4);
    tiers
}

/// Detect header level from font size using document-specific heading tiers.
/// When tiers are available, maps tier 0→H1, tier 1→H2, etc.
/// Falls back to ratio-based thresholds when no tiers exist.
pub(crate) fn detect_header_level(
    font_size: f32,
    base_size: f32,
    heading_tiers: &[f32],
) -> Option<usize> {
    let ratio = font_size / base_size;

    if ratio < 1.2 {
        return None; // Regular text
    }

    if !heading_tiers.is_empty() {
        // Match font_size to a tier (within 0.5pt tolerance)
        for (i, &tier_size) in heading_tiers.iter().enumerate() {
            if (font_size - tier_size).abs() < 0.5 {
                return Some(i + 1); // tier 0 → H1, tier 1 → H2, etc.
            }
        }
        // No tier match but large ratio — assign level after last tier
        if ratio >= 1.5 {
            let level = (heading_tiers.len() + 1).min(4);
            return Some(level);
        }
        // No tier match and small ratio — not a heading
        return None;
    }

    // Fallback: original ratio-based thresholds (no tiers discovered)
    if ratio >= 2.0 {
        Some(1)
    } else if ratio >= 1.5 {
        Some(2)
    } else if ratio >= 1.25 {
        Some(3)
    } else {
        Some(4)
    }
}
