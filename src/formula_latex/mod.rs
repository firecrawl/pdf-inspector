//! Heuristic LaTeX recovery from positioned PDF text items.
//!
//! Given a bounding box (from a layout model's "formula" detection) and the
//! positioned text items that fall within it, this module reconstructs
//! approximate LaTeX by analyzing:
//!
//! - Font size relative to the median → subscript / superscript detection
//! - Y position relative to the baseline → sub/super confirmation
//! - Unicode character identity → LaTeX command mapping
//! - Vertical text layout → simple fraction detection
//!
//! Phase 1 scope: inline formulas, simple sub/superscripts, simple fractions.
//! Complex structures (matrices, align environments, nested fractions, integral
//! bounds) are left to Phase 2 / GPU OCR fallback.

pub mod unicode_map;

use crate::text_utils;
use crate::types::TextItem;
use unicode_map::text_to_latex_chars;

// =========================================================================
// Public result types
// =========================================================================

/// Result for a single formula region's LaTeX reconstruction.
#[derive(Debug, Clone)]
pub struct FormulaResult {
    /// Reconstructed LaTeX string.
    pub latex: String,
    /// The linearized raw text (same as `extract_formulas_in_regions_mem` returns).
    pub raw_text: String,
    /// Heuristic confidence in the LaTeX output (0.0–1.0).
    pub confidence: f32,
    /// True when extraction failed entirely (empty, garbage) and GPU OCR is needed.
    pub needs_ocr: bool,
    /// Human-readable breakdown of confidence penalties (empty if no penalties fired).
    pub confidence_breakdown: Vec<String>,
}

/// Result for one page's formula regions.
#[derive(Debug, Clone)]
pub struct PageFormulaResult {
    /// 0-indexed page number.
    pub page: u32,
    /// Per-region results, parallel to the input regions.
    pub regions: Vec<FormulaResult>,
}

// =========================================================================
// Positioned item classification
// =========================================================================

/// Classification of a text item's vertical position relative to the formula baseline.
#[derive(Debug, Clone, Copy, PartialEq)]
enum VerticalRole {
    /// Normal baseline text.
    Inline,
    /// Subscript: smaller font, positioned below baseline.
    Subscript,
    /// Superscript: smaller font, positioned above baseline.
    Superscript,
}

/// A text item annotated with its vertical role and LaTeX-converted text.
#[derive(Debug, Clone)]
struct ClassifiedItem {
    /// LaTeX-converted text for this item.
    latex_text: String,
    /// Original raw text.
    raw_text: String,
    x: f32,
    y: f32,
    width: f32,
    font_size: f32,
    role: VerticalRole,
    /// Fraction of characters in this item that mapped to known LaTeX.
    char_known_frac: f32,
}

// =========================================================================
// Core reconstruction
// =========================================================================

/// Reconstruct LaTeX from positioned text items within a formula bounding box.
///
/// `items` should be pre-filtered to those whose center falls within the bbox.
/// Returns `(latex, raw_text, confidence, confidence_breakdown)`.
pub fn reconstruct_latex(items: &[TextItem]) -> (String, String, f32, Vec<String>) {
    if items.is_empty() {
        return (String::new(), String::new(), 0.0, Vec::new());
    }

    // Build raw text (same linearization as collect_text_in_region)
    let raw_text = linearize_items(items);

    // Single-item fast path
    if items.len() == 1 {
        let (latex, char_frac) = text_to_latex_chars(&items[0].text);
        let ctx = ConfidenceContext {
            items,
            char_known_frac: char_frac,
            positions_unambiguous: true,
            has_artifacts: has_encoding_artifacts(items),
            fraction_fired: false,
            fraction_numer_x_min: 0.0,
            fraction_numer_x_max: 0.0,
            fraction_denom_x_min: 0.0,
            fraction_denom_x_max: 0.0,
        };
        let (confidence, breakdown) = compute_confidence(&ctx);
        return (latex.trim().to_string(), raw_text, confidence, breakdown);
    }

    // Step 1: Compute baseline statistics
    let (median_font_size, median_y) = compute_baseline_stats(items);

    // Step 2: Classify each item (includes Unicode→LaTeX conversion)
    let classified = classify_items(items, median_font_size, median_y);

    // Aggregate char_known_frac across all items
    let total_chars: usize = classified.iter().map(|c| c.raw_text.len().max(1)).sum();
    let weighted_known: f32 = classified
        .iter()
        .map(|c| c.char_known_frac * c.raw_text.len().max(1) as f32)
        .sum();
    let overall_char_frac = if total_chars > 0 {
        weighted_known / total_chars as f32
    } else {
        1.0
    };

    // Step 3: Detect fractions (vertically stacked text with similar x-extent)
    // Only consider fractions when items of similar size exist both above and below
    let fraction_info = detect_fraction_geometry(&classified, median_y, median_font_size);
    if let Some(frac_latex) = try_detect_fraction(&classified, median_y, median_font_size) {
        let (frac_fired, n_xmin, n_xmax, d_xmin, d_xmax) = fraction_info;
        let ctx = ConfidenceContext {
            items,
            char_known_frac: overall_char_frac,
            positions_unambiguous: true,
            has_artifacts: has_encoding_artifacts(items),
            fraction_fired: frac_fired,
            fraction_numer_x_min: n_xmin,
            fraction_numer_x_max: n_xmax,
            fraction_denom_x_min: d_xmin,
            fraction_denom_x_max: d_xmax,
        };
        let (confidence, breakdown) = compute_confidence(&ctx);
        return (
            frac_latex.trim().to_string(),
            raw_text,
            confidence,
            breakdown,
        );
    }

    // Step 4: Walk items in reading order, emitting LaTeX with sub/super wrapping
    let latex = emit_latex_from_classified(&classified);

    // Step 5: Compute confidence
    let has_ambiguous_positions = classified
        .iter()
        .any(|c| c.role != VerticalRole::Inline && (c.font_size / median_font_size) > 0.80);

    let (frac_fired, n_xmin, n_xmax, d_xmin, d_xmax) = fraction_info;
    let ctx = ConfidenceContext {
        items,
        char_known_frac: overall_char_frac,
        positions_unambiguous: !has_ambiguous_positions,
        has_artifacts: has_encoding_artifacts(items),
        fraction_fired: frac_fired,
        fraction_numer_x_min: n_xmin,
        fraction_numer_x_max: n_xmax,
        fraction_denom_x_min: d_xmin,
        fraction_denom_x_max: d_xmax,
    };
    let (confidence, breakdown) = compute_confidence(&ctx);

    (latex.trim().to_string(), raw_text, confidence, breakdown)
}

/// Linearize items into plain text (reading order, newline between rows).
fn linearize_items(items: &[TextItem]) -> String {
    let mut sorted: Vec<&TextItem> = items.iter().collect();
    sorted.sort_by(|a, b| {
        // Sort top-to-bottom (higher y = higher on page in PDF coords), then left-to-right
        b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x))
    });

    let y_tolerance = 3.0;
    let mut lines: Vec<Vec<&TextItem>> = Vec::new();

    for item in sorted {
        let should_merge = lines.last().is_some_and(|last_line| {
            !last_line.is_empty() && (last_line[0].y - item.y).abs() < y_tolerance
        });
        if should_merge {
            lines.last_mut().unwrap().push(item);
        } else {
            lines.push(vec![item]);
        }
    }

    // Sort items within each line by x
    for line in &mut lines {
        line.sort_by(|a, b| a.x.total_cmp(&b.x));
    }

    lines
        .iter()
        .map(|line| {
            let mut s = String::new();
            for (i, item) in line.iter().enumerate() {
                if i > 0 {
                    let prev = line[i - 1];
                    let gap = item.x - (prev.x + text_utils::effective_width(prev));
                    if gap > prev.font_size * 0.15 {
                        s.push(' ');
                    }
                }
                s.push_str(&item.text);
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compute the "base" font size and baseline y position from items.
///
/// The base font size is determined by looking at the largest font size cluster.
/// In formulas, the main text is at the largest size; subscripts and superscripts
/// are at smaller sizes. Using the max (instead of median) prevents a formula
/// like `x_a^b` (one 12pt item, two 8pt items) from treating 8pt as "normal".
///
/// The baseline y is the median y of items at or near the base font size.
fn compute_baseline_stats(items: &[TextItem]) -> (f32, f32) {
    let max_font_size = items.iter().map(|i| i.font_size).fold(0.0f32, f32::max);

    // Use max font size as the reference. Items within 85% of this are "base size".
    let base_font_size = max_font_size;

    // For baseline, use the y position of items near the base font size.
    let baseline_ys: Vec<f32> = items
        .iter()
        .filter(|i| i.font_size >= base_font_size * 0.85)
        .map(|i| i.y)
        .collect();

    let median_y = if baseline_ys.is_empty() {
        // Fallback: use mean of all items
        let sum: f32 = items.iter().map(|i| i.y).sum();
        sum / items.len() as f32
    } else {
        // Use the mean y of baseline items. Mean is more robust than median
        // here because with 2 items (e.g., a fraction's numerator and
        // denominator both at normal font size), the median picks one of
        // them, while the mean falls between them.
        let sum: f32 = baseline_ys.iter().sum();
        sum / baseline_ys.len() as f32
    };

    (base_font_size, median_y)
}

/// Classify items as inline, subscript, or superscript based on font size and y position.
/// Also converts each item's text to LaTeX.
fn classify_items(items: &[TextItem], median_fs: f32, median_y: f32) -> Vec<ClassifiedItem> {
    let fs_threshold = 0.85;
    // Y tolerance: items within this distance of the median baseline are "inline"
    // even if they're slightly offset. Use a fraction of the median font size.
    let y_tolerance = median_fs * 0.25;

    let mut result: Vec<ClassifiedItem> = items
        .iter()
        .map(|item| {
            let is_small = item.font_size < median_fs * fs_threshold;
            let y_diff = item.y - median_y; // positive = above baseline in PDF coords

            let role = if is_small {
                if y_diff > y_tolerance {
                    VerticalRole::Superscript
                } else if y_diff < -y_tolerance {
                    VerticalRole::Subscript
                } else {
                    // Small but on the baseline — could be a symbol, treat as inline
                    VerticalRole::Inline
                }
            } else {
                VerticalRole::Inline
            };

            let (latex_text, char_known_frac) = text_to_latex_chars(&item.text);

            ClassifiedItem {
                latex_text,
                raw_text: item.text.clone(),
                x: item.x,
                y: item.y,
                width: text_utils::effective_width(item),
                font_size: item.font_size,
                role,
                char_known_frac,
            }
        })
        .collect();

    // Sort by x position (reading order within the formula)
    result.sort_by(|a, b| a.x.total_cmp(&b.x));

    result
}

/// Try to detect a simple fraction pattern: two horizontal text rows,
/// one above and one below the median baseline, with similar x-extent.
///
/// Requirements for fraction detection:
/// - Items both above and below the baseline (at similar font sizes)
/// - The above and below groups must overlap in x-extent
/// - No subscript/superscript items (those are a different pattern)
///
/// Returns the reconstructed LaTeX (with Unicode already converted) if detected.
fn try_detect_fraction(items: &[ClassifiedItem], median_y: f32, median_fs: f32) -> Option<String> {
    if items.len() < 2 {
        return None;
    }

    // Don't try fraction detection if there are any sub/superscript items
    // (that indicates inline formula with sub/super, not a fraction)
    if items.iter().any(|i| i.role != VerticalRole::Inline) {
        return None;
    }

    let y_gap_threshold = median_fs * 0.5;

    // Separate items into above-baseline and below-baseline groups
    // Only consider items whose font size is in the "normal" range
    let above: Vec<&ClassifiedItem> = items
        .iter()
        .filter(|i| i.y > median_y + y_gap_threshold)
        .collect();
    let below: Vec<&ClassifiedItem> = items
        .iter()
        .filter(|i| i.y < median_y - y_gap_threshold)
        .collect();
    let on_baseline: Vec<&ClassifiedItem> = items
        .iter()
        .filter(|i| (i.y - median_y).abs() <= y_gap_threshold)
        .collect();

    // For a fraction: we need items both above and below
    if above.is_empty() || below.is_empty() {
        return None;
    }

    // Check that above and below groups have similar x-extent
    let above_x_min = above.iter().map(|i| i.x).fold(f32::MAX, f32::min);
    let above_x_max = above.iter().map(|i| i.x + i.width).fold(f32::MIN, f32::max);
    let below_x_min = below.iter().map(|i| i.x).fold(f32::MAX, f32::min);
    let below_x_max = below.iter().map(|i| i.x + i.width).fold(f32::MIN, f32::max);

    let above_width = above_x_max - above_x_min;
    let below_width = below_x_max - below_x_min;

    // x-extents should overlap significantly
    let overlap_min = above_x_min.max(below_x_min);
    let overlap_max = above_x_max.min(below_x_max);
    let overlap = (overlap_max - overlap_min).max(0.0);
    let max_width = above_width.max(below_width).max(1.0);

    if overlap / max_width < 0.3 {
        return None;
    }

    // Build the fraction text (already LaTeX-converted)
    let mut numer_items: Vec<&ClassifiedItem> = above;
    numer_items.sort_by(|a, b| a.x.total_cmp(&b.x));
    let numerator = items_to_latex(&numer_items);

    let mut denom_items: Vec<&ClassifiedItem> = below;
    denom_items.sort_by(|a, b| a.x.total_cmp(&b.x));
    let denominator = items_to_latex(&denom_items);

    if numerator.trim().is_empty() || denominator.trim().is_empty() {
        return None;
    }

    // Build result with any baseline items before/after
    let mut result = String::new();
    let frac_x_min = above_x_min.min(below_x_min);
    let frac_x_max = above_x_max.max(below_x_max);

    // Items before the fraction
    let mut before: Vec<&ClassifiedItem> = on_baseline
        .iter()
        .filter(|i| i.x + i.width < frac_x_min)
        .copied()
        .collect();
    before.sort_by(|a, b| a.x.total_cmp(&b.x));
    for item in &before {
        result.push_str(&item.latex_text);
        result.push(' ');
    }

    result.push_str(&format!(
        "\\frac{{{}}}{{{}}}",
        numerator.trim(),
        denominator.trim()
    ));

    // Items after the fraction
    let mut after: Vec<&ClassifiedItem> = on_baseline
        .iter()
        .filter(|i| i.x > frac_x_max)
        .copied()
        .collect();
    after.sort_by(|a, b| a.x.total_cmp(&b.x));
    for item in &after {
        result.push(' ');
        result.push_str(&item.latex_text);
    }

    Some(result)
}

/// Convert a sorted list of classified items into a LaTeX string with spacing.
fn items_to_latex(items: &[&ClassifiedItem]) -> String {
    let mut s = String::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            let prev = items[i - 1];
            let gap = item.x - (prev.x + prev.width);
            if gap > prev.font_size * 0.15 {
                s.push(' ');
            }
        }
        s.push_str(&item.latex_text);
    }
    s
}

/// Emit LaTeX from classified items, wrapping sub/superscript runs in `_{}`/`^{}`.
fn emit_latex_from_classified(items: &[ClassifiedItem]) -> String {
    if items.is_empty() {
        return String::new();
    }

    let mut result = String::new();
    let mut current_role = VerticalRole::Inline;
    let mut group_text = String::new();

    for (i, item) in items.iter().enumerate() {
        // Check if we need spacing from the previous item
        let needs_space = if i > 0 {
            let prev = &items[i - 1];
            let gap = item.x - (prev.x + prev.width);
            // Only add space for significant gaps between items of the same role
            gap > prev.font_size * 0.3
                && item.role == VerticalRole::Inline
                && prev.role == VerticalRole::Inline
        } else {
            false
        };

        if item.role != current_role {
            // Flush the current group
            flush_group(&mut result, &group_text, current_role);
            group_text.clear();
            current_role = item.role;
        }

        if needs_space && group_text.is_empty() {
            result.push(' ');
        } else if needs_space {
            group_text.push(' ');
        }

        // Add spacing within the same role group
        if !group_text.is_empty() && i > 0 {
            let prev = &items[i - 1];
            if prev.role == item.role {
                let gap = item.x - (prev.x + prev.width);
                if gap > prev.font_size * 0.15 {
                    group_text.push(' ');
                }
            }
        }

        group_text.push_str(&item.latex_text);
    }

    // Flush final group
    flush_group(&mut result, &group_text, current_role);

    result
}

/// Flush a sub/superscript group into the result string.
fn flush_group(result: &mut String, text: &str, role: VerticalRole) {
    if text.is_empty() {
        return;
    }
    match role {
        VerticalRole::Inline => result.push_str(text),
        VerticalRole::Subscript => {
            let trimmed = text.trim();
            if trimmed.chars().count() == 1 {
                result.push('_');
                result.push_str(trimmed);
            } else {
                result.push_str("_{");
                result.push_str(trimmed);
                result.push('}');
            }
        }
        VerticalRole::Superscript => {
            let trimmed = text.trim();
            if trimmed.chars().count() == 1 {
                result.push('^');
                result.push_str(trimmed);
            } else {
                result.push_str("^{");
                result.push_str(trimmed);
                result.push('}');
            }
        }
    }
}

// =========================================================================
// Confidence scoring
// =========================================================================

/// Context for computing confidence — captures heuristic decisions made during
/// reconstruction so that penalties can reference them.
struct ConfidenceContext<'a> {
    items: &'a [TextItem],
    char_known_frac: f32,
    positions_unambiguous: bool,
    has_artifacts: bool,
    /// Whether fraction detection fired (try_detect_fraction returned Some).
    fraction_fired: bool,
    /// Numerator x-extent (only meaningful when fraction_fired is true).
    fraction_numer_x_min: f32,
    fraction_numer_x_max: f32,
    /// Denominator x-extent (only meaningful when fraction_fired is true).
    fraction_denom_x_min: f32,
    fraction_denom_x_max: f32,
}

/// Characters considered "huge operators" for the PENALTY_HUGE_OPERATOR check.
const HUGE_OPERATOR_CHARS: &[char] = &[
    '\u{222B}', // ∫
    '\u{2211}', // ∑
    '\u{220F}', // ∏
    '\u{221A}', // √
    '\u{222E}', // ∮
    '\u{222F}', // ∯
    '\u{2230}', // ∰
];

/// Compute confidence score (0.0–1.0) for the reconstructed LaTeX.
///
/// Returns `(score, breakdown)` where breakdown lists penalties that fired.
fn compute_confidence(ctx: &ConfidenceContext<'_>) -> (f32, Vec<String>) {
    let mut score: f32 = 0.0;
    let mut breakdown: Vec<String> = Vec::new();
    let items = ctx.items;

    // ── Positive points (unchanged from Phase 1) ────────────────────

    // +0.3 if every char mapped to known LaTeX
    score += 0.3 * ctx.char_known_frac;

    // +0.2 if all sub/super positions detected unambiguously
    if ctx.positions_unambiguous {
        score += 0.2;
    }

    // +0.2 if no horizontal layout ambiguity
    let item_count = items.len();
    if item_count <= 10 {
        score += 0.2;
    } else if item_count <= 20 {
        score += 0.1;
    }

    // +0.2 if item count <= 30 (simple inline formula)
    if item_count <= 30 {
        score += 0.2;
    } else {
        score += 0.2 * (30.0 / item_count as f32).min(1.0);
    }

    // +0.1 if no encoding artifacts
    if !ctx.has_artifacts {
        score += 0.1;
    }

    let base_score = score;

    // ── Penalty terms ───────────────────────────────────────────────

    // PENALTY_MANY_ITEMS: complex formulas with many items are error-prone
    if item_count > 15 {
        score -= 0.20;
        breakdown.push(format!("MANY_ITEMS({item_count}>15): -0.20"));
    }
    // PENALTY_VERY_MANY_ITEMS: additional penalty for very large formulas
    if item_count > 25 {
        score -= 0.10;
        breakdown.push(format!("VERY_MANY_ITEMS({item_count}>25): -0.10"));
    }

    // PENALTY_MULTI_BAND: 3+ distinct y-clusters means multi-line equation
    let max_fs = items.iter().map(|i| i.font_size).fold(0.0f32, f32::max);
    let y_cluster_count = count_y_clusters(items, max_fs);
    if y_cluster_count >= 3 {
        score -= 0.30;
        breakdown.push(format!("MULTI_BAND({y_cluster_count}>=3): -0.30"));
    }

    // PENALTY_FRACTION_MISMATCH: fraction detection fired but geometry is suspicious
    if ctx.fraction_fired {
        let numer_width = (ctx.fraction_numer_x_max - ctx.fraction_numer_x_min).max(0.001);
        let denom_width = ctx.fraction_denom_x_max - ctx.fraction_denom_x_min;

        // Denominator extends much wider than numerator
        if denom_width > 1.5 * numer_width {
            score -= 0.30;
            breakdown.push(format!(
                "FRAC_MISMATCH(denom_w={denom_width:.1}>1.5*numer_w={numer_width:.1}): -0.30"
            ));
        }

        // Denominator starts well to the left of numerator
        let left_overshoot = ctx.fraction_numer_x_min - ctx.fraction_denom_x_min;
        if left_overshoot > 0.5 * numer_width {
            score -= 0.30;
            breakdown.push(format!(
                "FRAC_LEFT_SHIFT(overshoot={left_overshoot:.1}>0.5*numer_w={numer_width:.1}): -0.30"
            ));
        }
    }

    // PENALTY_HUGE_OPERATOR: oversized integral/sum/prod with surrounding items
    if items.len() > 1 {
        let median_fs = median_font_size_of(items);
        let has_huge_op = items.iter().any(|i| {
            i.font_size > 1.3 * median_fs
                && i.text.chars().any(|ch| HUGE_OPERATOR_CHARS.contains(&ch))
        });
        if has_huge_op {
            score -= 0.30;
            breakdown.push("HUGE_OPERATOR: -0.30".to_string());
        }
    }

    // PENALTY_FONT_SIZE_VARIANCE: items in the same y-band have >2 distinct font sizes
    if has_font_size_variance_in_band(items, max_fs) {
        score -= 0.15;
        breakdown.push("FONT_SIZE_VARIANCE: -0.15".to_string());
    }

    if !breakdown.is_empty() {
        breakdown.insert(
            0,
            format!("base={base_score:.2}, final={:.2}", score.clamp(0.0, 1.0)),
        );
    }

    (score.clamp(0.0, 1.0), breakdown)
}

/// Count distinct y-clusters among items. Two items are in the same cluster if
/// their y-centers are within `0.5 * max_font_size` of each other.
fn count_y_clusters(items: &[TextItem], max_fs: f32) -> usize {
    if items.is_empty() {
        return 0;
    }
    let threshold = 0.5 * max_fs;

    // Collect y-centers and sort
    let mut ys: Vec<f32> = items.iter().map(|i| i.y + i.height / 2.0).collect();
    ys.sort_by(|a, b| a.total_cmp(b));

    let mut clusters = 1usize;
    let mut cluster_y = ys[0];
    for &y in &ys[1..] {
        if (y - cluster_y).abs() > threshold {
            clusters += 1;
            cluster_y = y;
        }
    }
    clusters
}

/// Compute the true median font size of items.
fn median_font_size_of(items: &[TextItem]) -> f32 {
    if items.is_empty() {
        return 0.0;
    }
    let mut sizes: Vec<f32> = items.iter().map(|i| i.font_size).collect();
    sizes.sort_by(|a, b| a.total_cmp(b));
    let mid = sizes.len() / 2;
    if sizes.len().is_multiple_of(2) {
        (sizes[mid - 1] + sizes[mid]) / 2.0
    } else {
        sizes[mid]
    }
}

/// Check if any y-band has >2 distinct font sizes among its items.
///
/// A y-band is a group of items with y-centers within `0.5 * max_fs` of each other.
fn has_font_size_variance_in_band(items: &[TextItem], max_fs: f32) -> bool {
    if items.len() < 3 {
        return false;
    }
    let threshold = 0.5 * max_fs;
    // Quantize font sizes to nearest 0.5pt to avoid floating-point noise
    let quantize = |fs: f32| -> i32 { (fs * 2.0).round() as i32 };

    // Sort by y-center
    let mut sorted: Vec<(f32, f32)> = items
        .iter()
        .map(|i| (i.y + i.height / 2.0, i.font_size))
        .collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Slide through sorted items, grouping by y proximity
    let mut band_start = 0;
    for i in 1..=sorted.len() {
        let end_of_band =
            i == sorted.len() || (sorted[i].0 - sorted[band_start].0).abs() > threshold;
        if end_of_band {
            // Count distinct font sizes in this band
            let mut sizes_in_band: Vec<i32> = sorted[band_start..i]
                .iter()
                .map(|(_, fs)| quantize(*fs))
                .collect();
            sizes_in_band.sort();
            sizes_in_band.dedup();
            if sizes_in_band.len() > 2 {
                return true;
            }
            band_start = i;
        }
    }
    false
}

/// Detect fraction geometry without actually building the LaTeX.
///
/// Returns `(fraction_fired, numer_x_min, numer_x_max, denom_x_min, denom_x_max)`.
/// If no fraction-like geometry is detected, returns `(false, 0, 0, 0, 0)`.
fn detect_fraction_geometry(
    items: &[ClassifiedItem],
    median_y: f32,
    median_fs: f32,
) -> (bool, f32, f32, f32, f32) {
    let no_frac = (false, 0.0, 0.0, 0.0, 0.0);
    if items.len() < 2 {
        return no_frac;
    }
    // Same criteria as try_detect_fraction: all items must be Inline
    if items.iter().any(|i| i.role != VerticalRole::Inline) {
        return no_frac;
    }

    let y_gap_threshold = median_fs * 0.5;
    let above: Vec<&ClassifiedItem> = items
        .iter()
        .filter(|i| i.y > median_y + y_gap_threshold)
        .collect();
    let below: Vec<&ClassifiedItem> = items
        .iter()
        .filter(|i| i.y < median_y - y_gap_threshold)
        .collect();

    if above.is_empty() || below.is_empty() {
        return no_frac;
    }

    let n_xmin = above.iter().map(|i| i.x).fold(f32::MAX, f32::min);
    let n_xmax = above.iter().map(|i| i.x + i.width).fold(f32::MIN, f32::max);
    let d_xmin = below.iter().map(|i| i.x).fold(f32::MAX, f32::min);
    let d_xmax = below.iter().map(|i| i.x + i.width).fold(f32::MIN, f32::max);

    (true, n_xmin, n_xmax, d_xmin, d_xmax)
}

/// Check if any items contain encoding artifacts (PUA chars, control chars, replacement char).
fn has_encoding_artifacts(items: &[TextItem]) -> bool {
    for item in items {
        for ch in item.text.chars() {
            let cp = ch as u32;
            if (0xE000..=0xF8FF).contains(&cp) {
                return true;
            }
            if cp < 0x20 && cp != 0x09 && cp != 0x0A && cp != 0x0D {
                return true;
            }
            if cp == 0xFFFD {
                return true;
            }
        }
    }
    false
}

// =========================================================================
// Filtering items to a bounding box
// =========================================================================

/// Filter text items to those whose center falls inside a bounding box.
///
/// The bbox is in the same coordinate system as the items (PDF bottom-left origin,
/// after any rotation normalization).
pub fn filter_items_in_bbox(
    items: &[TextItem],
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
) -> Vec<TextItem> {
    items
        .iter()
        .filter(|item| {
            let cx = item.x + text_utils::effective_width(item) / 2.0;
            let cy = item.y + item.height / 2.0;
            cx >= x_min && cx <= x_max && cy >= y_min && cy <= y_max
        })
        .cloned()
        .collect()
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    fn make_item(text: &str, x: f32, y: f32, font_size: f32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width: text.len() as f32 * font_size * 0.5,
            height: font_size,
            font: "TestFont".to_string(),
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn test_simple_inline() {
        let items = vec![
            make_item("x", 10.0, 100.0, 12.0),
            make_item("+", 22.0, 100.0, 12.0),
            make_item("y", 34.0, 100.0, 12.0),
            make_item("=", 46.0, 100.0, 12.0),
            make_item("z", 58.0, 100.0, 12.0),
        ];
        let (latex, _raw, confidence, _breakdown) = reconstruct_latex(&items);
        assert!(latex.contains('x'), "expected x in: {}", latex);
        assert!(latex.contains('y'), "expected y in: {}", latex);
        assert!(latex.contains('z'), "expected z in: {}", latex);
        assert!(latex.contains('+'), "expected + in: {}", latex);
        assert!(latex.contains('='), "expected = in: {}", latex);
        assert!(
            confidence > 0.8,
            "confidence={} for simple inline",
            confidence
        );
    }

    #[test]
    fn test_subscript_detection() {
        // "x_2" — x at normal size, 2 at smaller size below baseline
        let items = vec![
            make_item("x", 10.0, 100.0, 12.0),
            make_item("2", 17.0, 95.0, 8.0),
        ];
        let (latex, _raw, _confidence, _breakdown) = reconstruct_latex(&items);
        assert!(latex.contains('_'), "expected subscript in: {}", latex);
        assert!(latex.contains('2'), "expected 2 in: {}", latex);
    }

    #[test]
    fn test_superscript_detection() {
        // "x^2" — x at normal size, 2 at smaller size above baseline
        let items = vec![
            make_item("x", 10.0, 100.0, 12.0),
            make_item("2", 17.0, 106.0, 8.0),
        ];
        let (latex, _raw, _confidence, _breakdown) = reconstruct_latex(&items);
        assert!(latex.contains('^'), "expected superscript in: {}", latex);
        assert!(latex.contains('2'), "expected 2 in: {}", latex);
    }

    #[test]
    fn test_subscript_multi_char() {
        // "x_{12}" — multi-character subscript should be wrapped in braces
        let items = vec![
            make_item("x", 10.0, 100.0, 12.0),
            make_item("12", 17.0, 95.0, 8.0),
        ];
        let (latex, _raw, _confidence, _breakdown) = reconstruct_latex(&items);
        assert!(latex.contains("_{12}"), "expected _{{12}} in: {}", latex);
    }

    #[test]
    fn test_fraction_detection() {
        // A simple fraction: "a" on top, "b" on bottom, same font size
        // With only 2 items at the same size, median_y falls between them.
        // We need a clear vertical separation.
        let items = vec![
            make_item("a", 20.0, 120.0, 10.0), // well above median
            make_item("b", 20.0, 80.0, 10.0),  // well below median
        ];
        let (latex, _raw, _confidence, _breakdown) = reconstruct_latex(&items);
        assert!(latex.contains("\\frac"), "expected \\frac in: {}", latex);
        assert!(
            latex.contains('a') && latex.contains('b'),
            "expected a and b in fraction: {}",
            latex
        );
    }

    #[test]
    fn test_fraction_with_surrounding_text() {
        // "= \frac{x}{y} +" — fraction with baseline items
        let items = vec![
            make_item("=", 10.0, 100.0, 10.0),
            make_item("x", 30.0, 120.0, 10.0), // numerator
            make_item("y", 30.0, 80.0, 10.0),  // denominator
            make_item("+", 50.0, 100.0, 10.0),
        ];
        let (latex, _raw, _confidence, _breakdown) = reconstruct_latex(&items);
        assert!(latex.contains("\\frac"), "expected \\frac in: {}", latex);
    }

    #[test]
    fn test_greek_conversion() {
        let items = vec![
            make_item("\u{03B1}", 10.0, 100.0, 12.0), // alpha
            make_item("+", 22.0, 100.0, 12.0),
            make_item("\u{03B2}", 34.0, 100.0, 12.0), // beta
            make_item("=", 46.0, 100.0, 12.0),
            make_item("\u{03B3}", 58.0, 100.0, 12.0), // gamma
        ];
        let (latex, _raw, confidence, _breakdown) = reconstruct_latex(&items);
        assert!(latex.contains(r"\alpha"), "expected \\alpha in: {}", latex);
        assert!(latex.contains(r"\beta"), "expected \\beta in: {}", latex);
        assert!(latex.contains(r"\gamma"), "expected \\gamma in: {}", latex);
        assert!(confidence > 0.8, "confidence={}", confidence);
    }

    #[test]
    fn test_empty_items() {
        let (latex, raw, confidence, _breakdown) = reconstruct_latex(&[]);
        assert!(latex.is_empty());
        assert!(raw.is_empty());
        assert!((confidence - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_single_item() {
        let items = vec![make_item("x", 10.0, 100.0, 12.0)];
        let (latex, _raw, confidence, _breakdown) = reconstruct_latex(&items);
        assert_eq!(latex, "x");
        assert!(confidence > 0.8);
    }

    #[test]
    fn test_confidence_low_for_unknown_chars() {
        let items = vec![make_item("\u{E000}\u{E001}\u{E002}", 10.0, 100.0, 12.0)];
        let (_latex, _raw, confidence, _breakdown) = reconstruct_latex(&items);
        assert!(
            confidence < 0.8,
            "confidence should be low for PUA chars: {}",
            confidence
        );
    }

    #[test]
    fn test_filter_items_in_bbox() {
        let items = vec![
            make_item("inside", 50.0, 50.0, 12.0),
            make_item("outside", 200.0, 200.0, 12.0),
        ];
        let filtered = filter_items_in_bbox(&items, 40.0, 40.0, 100.0, 70.0);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].text, "inside");
    }

    #[test]
    fn test_sub_super_combined() {
        // x_a^b — subscript and superscript on same base
        let items = vec![
            make_item("x", 10.0, 100.0, 12.0),
            make_item("a", 17.0, 95.0, 8.0),  // subscript
            make_item("b", 17.0, 106.0, 8.0), // superscript
        ];
        let (latex, _raw, _confidence, _breakdown) = reconstruct_latex(&items);
        assert!(latex.contains('_'), "expected subscript in: {}", latex);
        assert!(latex.contains('^'), "expected superscript in: {}", latex);
    }

    #[test]
    fn test_operator_conversion() {
        // "x ≤ ∞"
        let items = vec![
            make_item("x", 10.0, 100.0, 12.0),
            make_item("\u{2264}", 22.0, 100.0, 12.0), // ≤
            make_item("\u{221E}", 34.0, 100.0, 12.0), // ∞
        ];
        let (latex, _raw, _confidence, _breakdown) = reconstruct_latex(&items);
        assert!(latex.contains(r"\leq"), "expected \\leq in: {}", latex);
        assert!(latex.contains(r"\infty"), "expected \\infty in: {}", latex);
    }

    #[test]
    fn test_no_fraction_for_subsup() {
        // Items with different font sizes should not trigger fraction detection
        let items = vec![
            make_item("x", 10.0, 100.0, 12.0),
            make_item("2", 17.0, 106.0, 8.0), // superscript (small)
            make_item("i", 17.0, 95.0, 8.0),  // subscript (small)
        ];
        let (latex, _raw, _confidence, _breakdown) = reconstruct_latex(&items);
        assert!(
            !latex.contains("\\frac"),
            "should not detect fraction for sub/superscripts: {}",
            latex
        );
    }

    #[test]
    fn test_linearize_multiline() {
        let items = vec![
            make_item("top", 10.0, 120.0, 12.0),
            make_item("bot", 10.0, 100.0, 12.0),
        ];
        let raw = linearize_items(&items);
        assert!(raw.contains('\n'), "expected newline in multiline raw text");
        assert!(raw.starts_with("top"));
    }

    // =====================================================================
    // Confidence penalty tests
    // =====================================================================

    #[test]
    fn test_penalty_many_items_fires_above_15() {
        // 16 items on the same baseline — should trigger MANY_ITEMS penalty
        let items: Vec<TextItem> = (0..16)
            .map(|i| make_item("x", 10.0 + i as f32 * 8.0, 100.0, 12.0))
            .collect();
        let (_latex, _raw, confidence, breakdown) = reconstruct_latex(&items);
        let has_many = breakdown.iter().any(|s| s.contains("MANY_ITEMS"));
        assert!(
            has_many,
            "MANY_ITEMS should fire for 16 items; breakdown={:?}",
            breakdown
        );
        // Confidence must be lower than a 10-item equivalent
        assert!(
            confidence < 0.80,
            "confidence={} should be < 0.80",
            confidence
        );
    }

    #[test]
    fn test_penalty_many_items_does_not_fire_at_15() {
        // 15 items — should NOT trigger MANY_ITEMS penalty
        let items: Vec<TextItem> = (0..15)
            .map(|i| make_item("x", 10.0 + i as f32 * 8.0, 100.0, 12.0))
            .collect();
        let (_latex, _raw, _confidence, breakdown) = reconstruct_latex(&items);
        let has_many = breakdown.iter().any(|s| s.contains("MANY_ITEMS"));
        assert!(
            !has_many,
            "MANY_ITEMS should NOT fire for 15 items; breakdown={:?}",
            breakdown
        );
    }

    #[test]
    fn test_penalty_very_many_items_fires_above_25() {
        // 26 items — should trigger both MANY_ITEMS and VERY_MANY_ITEMS
        let items: Vec<TextItem> = (0..26)
            .map(|i| make_item("a", 10.0 + i as f32 * 8.0, 100.0, 12.0))
            .collect();
        let (_latex, _raw, confidence, breakdown) = reconstruct_latex(&items);
        let has_very_many = breakdown.iter().any(|s| s.contains("VERY_MANY_ITEMS"));
        assert!(
            has_very_many,
            "VERY_MANY_ITEMS should fire for 26 items; breakdown={:?}",
            breakdown
        );
        // Total penalty: -0.20 (MANY) + -0.10 (VERY_MANY) = -0.30
        assert!(
            confidence < 0.70,
            "confidence={} should be < 0.70",
            confidence
        );
    }

    #[test]
    fn test_penalty_multi_band_fires_for_3_clusters() {
        // 3 distinct y-bands at y=80, y=100, y=120 (font_size 10, threshold = 5)
        // bands are separated by 20 > 0.5*10=5
        let items = vec![
            make_item("a", 10.0, 120.0, 10.0),
            make_item("b", 10.0, 100.0, 10.0),
            make_item("c", 10.0, 80.0, 10.0),
        ];
        let (_latex, _raw, confidence, breakdown) = reconstruct_latex(&items);
        let has_multi = breakdown.iter().any(|s| s.contains("MULTI_BAND"));
        assert!(
            has_multi,
            "MULTI_BAND should fire for 3 y-clusters; breakdown={:?}",
            breakdown
        );
        assert!(
            confidence <= 0.70,
            "confidence={} should be <= 0.70",
            confidence
        );
    }

    #[test]
    fn test_penalty_multi_band_does_not_fire_for_2_clusters() {
        // 2 y-bands — should NOT trigger MULTI_BAND
        let items = vec![
            make_item("a", 10.0, 120.0, 10.0),
            make_item("b", 10.0, 80.0, 10.0),
        ];
        let (_latex, _raw, _confidence, breakdown) = reconstruct_latex(&items);
        let has_multi = breakdown.iter().any(|s| s.contains("MULTI_BAND"));
        assert!(
            !has_multi,
            "MULTI_BAND should NOT fire for 2 y-clusters; breakdown={:?}",
            breakdown
        );
    }

    #[test]
    fn test_penalty_huge_operator_fires() {
        // A giant integral (font_size 20) with normal items (font_size 10)
        // median font_size = 10, 20 > 1.3 * 10 = 13
        let items = vec![
            make_item("x", 10.0, 100.0, 10.0),
            make_item("\u{222B}", 20.0, 100.0, 20.0), // ∫ at 2x size
            make_item("f", 35.0, 100.0, 10.0),
        ];
        let (_latex, _raw, confidence, breakdown) = reconstruct_latex(&items);
        let has_huge = breakdown.iter().any(|s| s.contains("HUGE_OPERATOR"));
        assert!(
            has_huge,
            "HUGE_OPERATOR should fire for oversized integral; breakdown={:?}",
            breakdown
        );
        assert!(
            confidence <= 0.70,
            "confidence={} should be <= 0.70",
            confidence
        );
    }

    #[test]
    fn test_penalty_huge_operator_does_not_fire_for_normal_size() {
        // An integral at the same font size as everything else — no penalty
        let items = vec![
            make_item("x", 10.0, 100.0, 12.0),
            make_item("\u{222B}", 20.0, 100.0, 12.0), // ∫ at same size
            make_item("f", 35.0, 100.0, 12.0),
        ];
        let (_latex, _raw, _confidence, breakdown) = reconstruct_latex(&items);
        let has_huge = breakdown.iter().any(|s| s.contains("HUGE_OPERATOR"));
        assert!(
            !has_huge,
            "HUGE_OPERATOR should NOT fire when integral is normal size; breakdown={:?}",
            breakdown
        );
    }

    #[test]
    fn test_penalty_font_size_variance_in_band() {
        // 4 items on the same y-band with 3 distinct font sizes (10, 12, 14)
        let items = vec![
            make_item("a", 10.0, 100.0, 10.0),
            make_item("b", 20.0, 100.0, 12.0),
            make_item("c", 30.0, 100.0, 14.0),
            make_item("d", 40.0, 100.0, 10.0),
        ];
        let (_latex, _raw, confidence, breakdown) = reconstruct_latex(&items);
        let has_var = breakdown.iter().any(|s| s.contains("FONT_SIZE_VARIANCE"));
        assert!(
            has_var,
            "FONT_SIZE_VARIANCE should fire for >2 distinct sizes in a band; breakdown={:?}",
            breakdown
        );
        assert!(
            confidence <= 0.85,
            "confidence={} should be <= 0.85",
            confidence
        );
    }
}
