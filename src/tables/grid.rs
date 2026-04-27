//! Column/row boundary detection and cell assignment for heuristic tables.

use crate::types::TextItem;

use super::{Table, TableDetectionMode};

pub(crate) fn find_column_boundaries(
    items: &[(usize, &TextItem)],
    mode: TableDetectionMode,
) -> Vec<f32> {
    let mut x_positions: Vec<f32> = items.iter().map(|(_, i)| i.x).collect();
    x_positions.sort_by(|a, b| a.total_cmp(b));

    if x_positions.is_empty() {
        return vec![];
    }

    // For dense, narrow-column tables (e.g. train schedules with 24 cols at
    // 26pt spacing), the old avg_gap approach over-clusters because avg_gap is
    // dominated by the many items *within* each column.  Use a gap-histogram
    // on consecutive position gaps to detect when columns are densely packed,
    // and only then lower the threshold below 25pt.
    let x_range = x_positions.last().unwrap() - x_positions.first().unwrap();
    let avg_gap = if x_positions.len() > 1 {
        x_range / (x_positions.len() - 1) as f32
    } else {
        60.0
    };

    // Default: original avg_gap approach, center-based clustering
    let mut cluster_threshold = avg_gap.clamp(25.0, 50.0);
    let mut use_edge_clustering = false;

    // Analyze the distribution of non-trivial consecutive gaps to detect
    // a bimodal pattern (small within-column gaps vs large between-column gaps).
    // When detected, switch to edge-based clustering with the lower threshold
    // to correctly separate densely-packed columns without over-splitting
    // wide columns (edge-based avoids the center-drift problem).
    let mut consec_gaps: Vec<f32> = x_positions
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|&g| g > 0.1) // skip near-duplicate positions
        .collect();

    if consec_gaps.len() > 2 {
        consec_gaps.sort_by(|a, b| a.total_cmp(b));
        // Find the biggest jump in the sorted gap sequence — natural break
        // between within-column jitter and between-column spacing.
        // Require at least 3 values on each side to avoid outlier-dominated
        // splits (e.g. a single large page-margin gap).
        let mut best_split = consec_gaps.len() / 2;
        let mut best_jump = 0.0f32;
        let min_side = 3.min(consec_gaps.len() / 2);
        for i in 0..consec_gaps.len().saturating_sub(1) {
            let left_count = i + 1;
            let right_count = consec_gaps.len() - i - 1;
            if left_count < min_side || right_count < min_side {
                continue;
            }
            let jump = consec_gaps[i + 1] - consec_gaps[i];
            if jump > best_jump {
                best_jump = jump;
                best_split = i;
            }
        }
        let threshold = (consec_gaps[best_split]
            + consec_gaps[(best_split + 1).min(consec_gaps.len() - 1)])
            / 2.0;
        // Override for tables with a clear bimodal gap pattern:
        // - Dense tables (500+ items, e.g. 24-column train schedule): use
        //   edge-based clustering with the detected threshold.
        // - Smaller tables with a strong bimodal signal (jump > 10pt):
        //   lower the threshold but keep center-based clustering to avoid
        //   over-splitting.
        if threshold < 15.0 && best_jump > 2.0 && x_positions.len() > 500 {
            cluster_threshold = threshold.clamp(8.0, 25.0);
            use_edge_clustering = true;
        } else if best_jump > 10.0 && threshold < cluster_threshold {
            // Strong bimodal signal even with fewer items — the gap between
            // within-column jitter and between-column spacing is unambiguous.
            cluster_threshold = threshold.max(8.0);
        }
    }

    // Track cluster membership: for each cluster, store the list of x positions
    let mut cluster_xs: Vec<Vec<f32>> = vec![vec![x_positions[0]]];

    for &x in &x_positions[1..] {
        let last_cluster = cluster_xs.last().unwrap();
        // For dense columns (gap-histogram triggered), use edge-based clustering:
        // compare with the last item to avoid center-drift that merges adjacent
        // narrow columns.  For normal tables, use center-based (original behavior).
        let reference = if use_edge_clustering {
            *last_cluster.last().unwrap()
        } else {
            last_cluster.iter().sum::<f32>() / last_cluster.len() as f32
        };

        if x - reference > cluster_threshold {
            cluster_xs.push(vec![x]);
        } else {
            cluster_xs.last_mut().unwrap().push(x);
        }
    }

    // Numeric column merge pass: when a sparse cluster (few items, typically
    // header text) is adjacent to a dense numeric cluster and within 1.5×
    // threshold, merge them. This fixes tables where multi-line wrapped
    // headers have slightly different X positions than the data columns,
    // causing the header and data to split into separate clusters.
    let columns_before_merge = cluster_xs.len();
    if columns_before_merge >= 3 {
        cluster_xs = merge_numeric_adjacent_clusters(cluster_xs, items, cluster_threshold);
    }

    let columns: Vec<f32> = cluster_xs
        .iter()
        .map(|xs| xs.iter().sum::<f32>() / xs.len() as f32)
        .collect();

    // Filter columns - each should have multiple items
    let min_items_per_col = (items.len() / columns.len().max(1) / 4).max(2);
    let columns: Vec<f32> = columns
        .into_iter()
        .filter(|&col_x| {
            items
                .iter()
                .filter(|(_, i)| (i.x - col_x).abs() < cluster_threshold)
                .count()
                >= min_items_per_col
        })
        .collect();

    log::debug!(
        "  find_column_boundaries: {} columns (merged from {}), threshold={:.1}, {} items",
        columns.len(),
        columns_before_merge,
        cluster_threshold,
        items.len()
    );

    // Anti-paragraph safeguard for BodyFont mode:
    // Paragraphs concentrate items at the left margin; tables distribute evenly.
    // Reject if any single column has >60% of all items.
    if mode == TableDetectionMode::BodyFont {
        let total_items = items.len();
        for &col_x in &columns {
            let count = items
                .iter()
                .filter(|(_, i)| (i.x - col_x).abs() < cluster_threshold)
                .count();
            if count as f32 / total_items as f32 > 0.60 {
                return vec![];
            }
        }
    }

    columns
}

/// Check if a text string looks like a number (digits, decimals, sign, comma).
fn is_numeric_text(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    // Match patterns like: 8.23, -1.05, 9.99, 7.12, 100, 3,456.78, +5%, ---
    // But NOT: BIO, Department, Core Courses
    s.chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == ',' || c == '-' || c == '+' || c == '%')
        && s.chars().any(|c| c.is_ascii_digit())
}

/// Merge adjacent X-position clusters when one is a sparse header cluster
/// and the other is a dense numeric data cluster. This prevents multi-line
/// wrapped headers from splitting a logical column into two clusters.
fn merge_numeric_adjacent_clusters(
    mut clusters: Vec<Vec<f32>>,
    items: &[(usize, &TextItem)],
    threshold: f32,
) -> Vec<Vec<f32>> {
    // For each cluster, compute: center, item count, numeric fraction
    struct ClusterInfo {
        center: f32,
        count: usize,
        numeric_frac: f32,
    }

    let compute_info = |xs: &[f32]| -> ClusterInfo {
        let center = xs.iter().sum::<f32>() / xs.len() as f32;
        // Count items and numeric fraction for items near this cluster center
        let mut total = 0;
        let mut numeric = 0;
        for (_, item) in items {
            if (item.x - center).abs() < threshold {
                total += 1;
                if is_numeric_text(&item.text) {
                    numeric += 1;
                }
            }
        }
        ClusterInfo {
            center,
            count: total,
            numeric_frac: if total > 0 {
                numeric as f32 / total as f32
            } else {
                0.0
            },
        }
    };

    // Merge distance: allow merging clusters that are slightly beyond the
    // original threshold. Use 1.5× threshold to catch header-vs-data splits.
    let merge_dist = threshold * 1.5;

    // Iterate and merge adjacent pairs. Use a simple left-to-right scan.
    let mut merged = true;
    while merged {
        merged = false;
        let mut i = 0;
        while i + 1 < clusters.len() {
            let info_a = compute_info(&clusters[i]);
            let info_b = compute_info(&clusters[i + 1]);
            let dist = (info_b.center - info_a.center).abs();

            if dist > merge_dist {
                i += 1;
                continue;
            }

            // Determine if one cluster is sparse (header) and the other
            // is dense and numeric (data). A cluster is "sparse" if it has
            // significantly fewer items than the other.
            let (sparse, dense) = if info_a.count < info_b.count {
                (&info_a, &info_b)
            } else {
                (&info_b, &info_a)
            };

            // Merge if the dense cluster is predominantly numeric (>50%)
            // and the sparse cluster has at most 1/3 the items of the dense one.
            let should_merge =
                dense.numeric_frac > 0.50 && sparse.count <= dense.count / 2 && sparse.count <= 5;

            if should_merge {
                log::debug!(
                    "  merging column clusters: center {:.1} ({} items, {:.0}% numeric) + {:.1} ({} items, {:.0}% numeric), dist={:.1}",
                    info_a.center,
                    info_a.count,
                    info_a.numeric_frac * 100.0,
                    info_b.center,
                    info_b.count,
                    info_b.numeric_frac * 100.0,
                    dist,
                );
                // Merge cluster i+1 into cluster i
                let next = clusters.remove(i + 1);
                clusters[i].extend(next);
                merged = true;
                // Don't increment i — check if the merged cluster can merge further
            } else {
                i += 1;
            }
        }
    }

    clusters
}

/// Find row boundaries by clustering Y positions
pub(crate) fn find_row_boundaries(items: &[(usize, &TextItem)]) -> Vec<f32> {
    let mut y_positions: Vec<f32> = items.iter().map(|(_, i)| i.y).collect();
    y_positions.sort_by(|a, b| b.total_cmp(a)); // Descending

    if y_positions.is_empty() {
        return vec![];
    }

    // Cluster Y positions - items within a fraction of the median font size are same row.
    // Using 0.8× median font keeps the threshold between intra-row gaps (~0pt) and
    // inter-row gaps (≥1× font size), preventing row merging in uniform-spaced PDFs.
    let cluster_threshold = {
        let mut font_sizes: Vec<f32> = items.iter().map(|(_, i)| i.font_size).collect();
        font_sizes.sort_by(|a, b| a.total_cmp(b));
        let median_font = font_sizes[font_sizes.len() / 2];
        (median_font * 0.8).max(4.0)
    };
    let mut rows = Vec::new();
    let mut cluster_items: Vec<f32> = vec![y_positions[0]];

    for &y in &y_positions[1..] {
        let cluster_center = cluster_items.iter().sum::<f32>() / cluster_items.len() as f32;

        if cluster_center - y >= cluster_threshold {
            // End current cluster (note: Y is descending)
            rows.push(cluster_center);
            cluster_items = vec![y];
        } else {
            cluster_items.push(y);
        }
    }

    if !cluster_items.is_empty() {
        rows.push(cluster_items.iter().sum::<f32>() / cluster_items.len() as f32);
    }

    rows
}

/// Find which column index an X position belongs to
pub(crate) fn find_column_index(columns: &[f32], x: f32) -> Option<usize> {
    // Calculate adaptive threshold based on column spacing
    let threshold = if columns.len() >= 2 {
        let min_gap = columns
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(f32::INFINITY, f32::min);
        (min_gap / 2.0).clamp(25.0, 50.0)
    } else {
        50.0
    };

    columns
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (x - *a)
                .abs()
                .partial_cmp(&(x - *b).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .filter(|(_, col_x)| (x - *col_x).abs() < threshold)
        .map(|(idx, _)| idx)
}

/// Find which row index a Y position belongs to
pub(crate) fn find_row_index(rows: &[f32], y: f32) -> Option<usize> {
    let threshold = 15.0;
    rows.iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (y - *a)
                .abs()
                .partial_cmp(&(y - *b).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .filter(|(_, row_y)| (y - *row_y).abs() < threshold)
        .map(|(idx, _)| idx)
}

/// Join cell items with subscript/superscript-aware spacing
/// Same logic as TextLine::text() but for table cells
pub(crate) fn join_cell_items(items: &[&TextItem]) -> String {
    let mut result = String::new();

    for (i, item) in items.iter().enumerate() {
        let text = item.text.trim();
        if text.is_empty() {
            continue;
        }

        if result.is_empty() {
            result.push_str(text);
        } else {
            let prev_item = items[i - 1];

            // Don't add space before/after hyphens
            let prev_ends_with_hyphen = result.ends_with('-');
            let curr_is_hyphen = text == "-";
            let curr_starts_with_hyphen = text.starts_with('-');

            // Detect subscript/superscript: smaller font size and/or Y offset
            let font_ratio = item.font_size / prev_item.font_size;
            let reverse_font_ratio = prev_item.font_size / item.font_size;
            let y_diff = (item.y - prev_item.y).abs();

            // Current item is subscript/superscript (smaller than previous)
            let is_sub_super = font_ratio < 0.85 && y_diff > 1.0;
            // Previous item was subscript/superscript (returning to normal size)
            let was_sub_super = reverse_font_ratio < 0.85 && y_diff > 1.0;

            if prev_ends_with_hyphen
                || curr_is_hyphen
                || curr_starts_with_hyphen
                || is_sub_super
                || was_sub_super
            {
                result.push_str(text);
            } else {
                result.push(' ');
                result.push_str(text);
            }
        }
    }

    result
}

/// Recover a header row for small-font tables by looking at body-font items
/// just above the table's first row.
///
/// PDF tables often have header rows at the body font size while data rows use
/// a smaller font. Pass 1 (SmallFont) excludes the header because of the
/// font-size filter. This function looks upward from the table's first row for
/// body-font items that align with the table's columns, and prepends them.
pub(crate) fn recover_header_row(
    table: &mut Table,
    all_items: &[TextItem],
    small_font_threshold: f32,
) {
    if table.rows.is_empty() || table.columns.is_empty() {
        return;
    }

    let first_row_y = table.rows[0]; // highest Y (rows are descending)

    // Compute typical row spacing for gap threshold
    let row_gap_limit = if table.rows.len() >= 2 {
        let avg_spacing =
            (table.rows[0] - table.rows[table.rows.len() - 1]) / (table.rows.len() - 1) as f32;
        // Allow up to 2x average row spacing for the header gap
        (avg_spacing * 2.0).clamp(10.0, 40.0)
    } else {
        30.0
    };

    // Find body-font items just above the first row
    let header_candidates: Vec<(usize, &TextItem)> = all_items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.font_size > small_font_threshold
                && item.y > first_row_y
                && item.y <= first_row_y + row_gap_limit
        })
        .collect();

    if header_candidates.is_empty() {
        return;
    }

    // Group header candidates by Y (cluster within 5pt)
    let mut header_y_groups: Vec<(f32, Vec<(usize, &TextItem)>)> = Vec::new();
    let mut sorted_candidates = header_candidates;
    sorted_candidates.sort_by(|a, b| {
        b.1.y
            .partial_cmp(&a.1.y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for (idx, item) in &sorted_candidates {
        let found = header_y_groups
            .iter_mut()
            .find(|(y, _)| (item.y - *y).abs() < 5.0);
        if let Some((_, group)) = found {
            group.push((*idx, item));
        } else {
            header_y_groups.push((item.y, vec![(*idx, item)]));
        }
    }

    // Take the row closest to the table (lowest Y above first_row_y)
    // header_y_groups is sorted by descending Y, so take the last one
    let (header_y, header_items) = header_y_groups.last().unwrap();

    // Map header items to table columns
    let num_cols = table.columns.len();
    let mut header_cells: Vec<String> = vec![String::new(); num_cols];
    let mut mapped_count = 0;
    let mut header_indices = Vec::new();

    for (idx, item) in header_items {
        if let Some(col) = find_column_index(&table.columns, item.x) {
            let text = item.text.trim();
            if !text.is_empty() {
                if !header_cells[col].is_empty() {
                    header_cells[col].push(' ');
                }
                header_cells[col].push_str(text);
                mapped_count += 1;
                header_indices.push(*idx);
            }
        }
    }

    // Require at least 2 columns populated to look like a real header row
    let populated = header_cells.iter().filter(|c| !c.is_empty()).count();
    if populated < 2 || mapped_count < 2 {
        return;
    }

    // Prepend header row to the table
    table.rows.insert(0, *header_y);
    table.cells.insert(0, header_cells);
    table.item_indices.extend(header_indices);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::TableKind;
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

    // --- find_column_index ---

    #[test]
    fn test_find_column_index_exact_match() {
        let columns = vec![100.0, 200.0, 300.0];
        assert_eq!(find_column_index(&columns, 100.0), Some(0));
        assert_eq!(find_column_index(&columns, 200.0), Some(1));
        assert_eq!(find_column_index(&columns, 300.0), Some(2));
    }

    #[test]
    fn test_find_column_index_closest_within_threshold() {
        let columns = vec![100.0, 200.0, 300.0];
        assert_eq!(find_column_index(&columns, 105.0), Some(0));
        assert_eq!(find_column_index(&columns, 195.0), Some(1));
    }

    #[test]
    fn test_find_column_index_outside_threshold() {
        let columns = vec![100.0, 200.0, 300.0];
        // Threshold is clamped to min 25, max 50 based on min gap / 2
        // Min gap = 100, threshold = clamp(50, 25, 50) = 50
        assert_eq!(find_column_index(&columns, 500.0), None);
    }

    #[test]
    fn test_find_column_index_single_column() {
        let columns = vec![150.0];
        // Single column → threshold = 50.0
        assert_eq!(find_column_index(&columns, 150.0), Some(0));
        assert_eq!(find_column_index(&columns, 170.0), Some(0));
    }

    #[test]
    fn test_find_column_index_empty_columns() {
        let columns: Vec<f32> = vec![];
        assert_eq!(find_column_index(&columns, 100.0), None);
    }

    // --- find_row_index ---

    #[test]
    fn test_find_row_index_exact_match() {
        let rows = vec![500.0, 480.0, 460.0];
        assert_eq!(find_row_index(&rows, 500.0), Some(0));
        assert_eq!(find_row_index(&rows, 480.0), Some(1));
    }

    #[test]
    fn test_find_row_index_within_threshold() {
        let rows = vec![500.0, 480.0, 460.0];
        assert_eq!(find_row_index(&rows, 505.0), Some(0));
        assert_eq!(find_row_index(&rows, 475.0), Some(1));
    }

    #[test]
    fn test_find_row_index_outside_threshold() {
        let rows = vec![500.0, 480.0, 460.0];
        // threshold is 15.0
        assert_eq!(find_row_index(&rows, 400.0), None);
    }

    #[test]
    fn test_find_row_index_single_row() {
        let rows = vec![500.0];
        assert_eq!(find_row_index(&rows, 500.0), Some(0));
        assert_eq!(find_row_index(&rows, 510.0), Some(0));
    }

    // --- find_column_boundaries ---

    #[test]
    fn test_find_column_boundaries_empty() {
        let items: Vec<(usize, &TextItem)> = vec![];
        assert_eq!(
            find_column_boundaries(&items, TableDetectionMode::SmallFont),
            vec![]
        );
    }

    #[test]
    fn test_find_column_boundaries_two_clusters() {
        // Items at x=100 and x=200 with enough repetition
        let items_data: Vec<TextItem> = (0..10)
            .map(|i| {
                let x = if i % 2 == 0 { 100.0 } else { 200.0 };
                make_item("Cell", x, 500.0 - (i as f32 * 20.0), 10.0)
            })
            .collect();
        let items: Vec<(usize, &TextItem)> = items_data.iter().enumerate().collect();
        let cols = find_column_boundaries(&items, TableDetectionMode::SmallFont);
        assert_eq!(cols.len(), 2);
    }

    #[test]
    fn test_find_column_boundaries_single_item() {
        let item = make_item("Solo", 100.0, 500.0, 10.0);
        let items: Vec<(usize, &TextItem)> = vec![(0, &item)];
        // Single item won't pass the min_items_per_col filter (needs >=2)
        let cols = find_column_boundaries(&items, TableDetectionMode::SmallFont);
        assert!(cols.is_empty());
    }

    #[test]
    fn test_find_column_boundaries_body_font_paragraph_rejection() {
        // All items at same X → >60% in one column → rejected in BodyFont mode
        let items_data: Vec<TextItem> = (0..10)
            .map(|i| make_item("Text", 100.0, 500.0 - (i as f32 * 20.0), 10.0))
            .collect();
        let items: Vec<(usize, &TextItem)> = items_data.iter().enumerate().collect();
        let cols = find_column_boundaries(&items, TableDetectionMode::BodyFont);
        assert!(cols.is_empty());
    }

    #[test]
    fn test_find_column_boundaries_min_items_filter() {
        // Create 10 items at x=100 and 1 item at x=300
        // The single outlier should be filtered out
        let mut items_data: Vec<TextItem> = (0..10)
            .map(|i| make_item("Cell", 100.0, 500.0 - (i as f32 * 20.0), 10.0))
            .collect();
        items_data.push(make_item("Lone", 300.0, 500.0, 10.0));
        let items: Vec<(usize, &TextItem)> = items_data.iter().enumerate().collect();
        let cols = find_column_boundaries(&items, TableDetectionMode::SmallFont);
        // Only the cluster at x=100 should survive
        assert!(cols.len() <= 1);
    }

    // --- find_row_boundaries ---

    #[test]
    fn test_find_row_boundaries_empty() {
        let items: Vec<(usize, &TextItem)> = vec![];
        assert_eq!(find_row_boundaries(&items), vec![]);
    }

    #[test]
    fn test_find_row_boundaries_descending_order() {
        let items_data = vec![
            make_item("A", 100.0, 500.0, 10.0),
            make_item("B", 100.0, 480.0, 10.0),
            make_item("C", 100.0, 460.0, 10.0),
        ];
        let items: Vec<(usize, &TextItem)> = items_data.iter().enumerate().collect();
        let rows = find_row_boundaries(&items);
        assert_eq!(rows.len(), 3);
        // Should be in descending order
        assert!(rows[0] > rows[1]);
        assert!(rows[1] > rows[2]);
    }

    #[test]
    fn test_find_row_boundaries_clustering() {
        // Items close together should cluster into one row
        let items_data = vec![
            make_item("A", 100.0, 500.0, 10.0),
            make_item("B", 200.0, 501.0, 10.0),
            make_item("C", 100.0, 480.0, 10.0),
        ];
        let items: Vec<(usize, &TextItem)> = items_data.iter().enumerate().collect();
        let rows = find_row_boundaries(&items);
        assert_eq!(rows.len(), 2); // 500 and 501 cluster together
    }

    #[test]
    fn test_find_row_boundaries_single_row() {
        let items_data = vec![make_item("A", 100.0, 500.0, 10.0)];
        let items: Vec<(usize, &TextItem)> = items_data.iter().enumerate().collect();
        let rows = find_row_boundaries(&items);
        assert_eq!(rows.len(), 1);
        assert!((rows[0] - 500.0).abs() < 0.01);
    }

    #[test]
    fn test_find_row_boundaries_items_at_same_y() {
        let items_data = vec![
            make_item("A", 100.0, 500.0, 10.0),
            make_item("B", 200.0, 500.0, 10.0),
            make_item("C", 300.0, 500.0, 10.0),
        ];
        let items: Vec<(usize, &TextItem)> = items_data.iter().enumerate().collect();
        let rows = find_row_boundaries(&items);
        assert_eq!(rows.len(), 1);
    }

    // --- join_cell_items ---

    #[test]
    fn test_join_cell_items_single_item() {
        let item = make_item("Hello", 100.0, 500.0, 10.0);
        assert_eq!(join_cell_items(&[&item]), "Hello");
    }

    #[test]
    fn test_join_cell_items_multiple_spaced() {
        let a = make_item("Hello", 100.0, 500.0, 10.0);
        let b = make_item("World", 150.0, 500.0, 10.0);
        assert_eq!(join_cell_items(&[&a, &b]), "Hello World");
    }

    #[test]
    fn test_join_cell_items_hyphen_no_space() {
        let a = make_item("pre", 100.0, 500.0, 10.0);
        let b = make_item("-", 120.0, 500.0, 10.0);
        let c = make_item("fix", 130.0, 500.0, 10.0);
        assert_eq!(join_cell_items(&[&a, &b, &c]), "pre-fix");
    }

    #[test]
    fn test_join_cell_items_subscript_no_space() {
        let a = make_item("H", 100.0, 500.0, 12.0);
        let b = make_item("2", 110.0, 497.0, 8.0); // smaller font, Y offset
        assert_eq!(join_cell_items(&[&a, &b]), "H2");
    }

    #[test]
    fn test_join_cell_items_empty_items_skipped() {
        let a = make_item("Hello", 100.0, 500.0, 10.0);
        let b = make_item("  ", 120.0, 500.0, 10.0);
        let c = make_item("World", 150.0, 500.0, 10.0);
        assert_eq!(join_cell_items(&[&a, &b, &c]), "Hello World");
    }

    // --- recover_header_row ---

    #[test]
    fn test_recover_header_row_prepends_header() {
        let all_items = vec![
            make_item("Col1", 100.0, 520.0, 12.0), // body font, above table
            make_item("Col2", 200.0, 520.0, 12.0), // body font, above table
            make_item("A", 100.0, 500.0, 8.0),     // small font, in table
            make_item("B", 200.0, 500.0, 8.0),
        ];
        let mut table = Table {
            columns: vec![100.0, 200.0],
            rows: vec![500.0, 480.0],
            cells: vec![vec!["A".into(), "B".into()], vec!["C".into(), "D".into()]],
            item_indices: vec![2, 3],
            kind: TableKind::Data,
        };

        recover_header_row(&mut table, &all_items, 9.0);
        assert_eq!(table.cells.len(), 3);
        assert_eq!(table.cells[0], vec!["Col1", "Col2"]);
    }

    #[test]
    fn test_recover_header_row_no_candidates() {
        let all_items = vec![
            make_item("A", 100.0, 500.0, 8.0),
            make_item("B", 200.0, 500.0, 8.0),
        ];
        let mut table = Table {
            columns: vec![100.0, 200.0],
            rows: vec![500.0],
            cells: vec![vec!["A".into(), "B".into()]],
            item_indices: vec![0, 1],
            kind: TableKind::Data,
        };

        let rows_before = table.rows.len();
        recover_header_row(&mut table, &all_items, 9.0);
        assert_eq!(table.rows.len(), rows_before);
    }

    #[test]
    fn test_recover_header_row_too_far_above() {
        let all_items = vec![
            make_item("Col1", 100.0, 600.0, 12.0), // way above
            make_item("Col2", 200.0, 600.0, 12.0),
            make_item("A", 100.0, 500.0, 8.0),
            make_item("B", 200.0, 500.0, 8.0),
        ];
        let mut table = Table {
            columns: vec![100.0, 200.0],
            rows: vec![500.0, 480.0],
            cells: vec![vec!["A".into(), "B".into()], vec!["C".into(), "D".into()]],
            item_indices: vec![2, 3],
            kind: TableKind::Data,
        };

        let rows_before = table.rows.len();
        recover_header_row(&mut table, &all_items, 9.0);
        assert_eq!(table.rows.len(), rows_before);
    }

    #[test]
    fn test_recover_header_row_single_column_populated() {
        // Only 1 column populated → not a real header
        let all_items = vec![
            make_item("OnlyCol1", 100.0, 520.0, 12.0),
            make_item("A", 100.0, 500.0, 8.0),
            make_item("B", 200.0, 500.0, 8.0),
        ];
        let mut table = Table {
            columns: vec![100.0, 200.0],
            rows: vec![500.0],
            cells: vec![vec!["A".into(), "B".into()]],
            item_indices: vec![1, 2],
            kind: TableKind::Data,
        };

        let rows_before = table.rows.len();
        recover_header_row(&mut table, &all_items, 9.0);
        assert_eq!(table.rows.len(), rows_before);
    }

    #[test]
    fn test_recover_header_row_empty_table() {
        let all_items = vec![make_item("Col1", 100.0, 520.0, 12.0)];
        let mut table = Table {
            columns: vec![],
            rows: vec![],
            cells: vec![],
            item_indices: vec![],
            kind: TableKind::Data,
        };

        recover_header_row(&mut table, &all_items, 9.0);
        assert!(table.cells.is_empty());
    }

    #[test]
    fn test_find_column_boundaries_dense_schedule() {
        // Simulate a 24-column train schedule with ~26pt column spacing and
        // per-glyph items that create many X-positions within each column.
        let mut items: Vec<(usize, TextItem)> = Vec::new();
        let mut rng_offset = 0.0f32;
        for col in 0..24 {
            let base_x = 50.0 + col as f32 * 26.0;
            // ~50 items per column with ±2pt jitter to simulate per-glyph text
            for row in 0..50 {
                rng_offset = (rng_offset + 0.7) % 4.0; // deterministic pseudo-jitter
                let x = base_x + rng_offset - 2.0;
                let y = 700.0 - row as f32 * 12.0;
                items.push((
                    0,
                    TextItem {
                        text: format!("{}", row),
                        x,
                        y,
                        width: 8.0,
                        font_size: 7.0,
                        height: 7.0,
                        font: String::new(),
                        is_bold: false,
                        is_italic: false,
                        item_type: ItemType::Text,
                        mcid: None,
                        page: 1,
                    },
                ));
            }
        }
        let refs: Vec<(usize, &TextItem)> = items.iter().map(|(i, t)| (*i, t)).collect();
        let cols = find_column_boundaries(&refs, TableDetectionMode::SmallFont);
        // Should find close to 24 columns (within ±2)
        assert!(
            cols.len() >= 22 && cols.len() <= 26,
            "Expected ~24 columns, got {}",
            cols.len()
        );
    }

    #[test]
    fn test_find_column_boundaries_wide_spacing_still_works() {
        // Normal table with 4 widely-spaced columns — should still work
        let mut items = Vec::new();
        for col in 0..4 {
            let base_x = 50.0 + col as f32 * 120.0;
            for row in 0..10 {
                items.push((
                    0,
                    TextItem {
                        text: format!("cell_{}_{}", col, row),
                        x: base_x + (row as f32 * 0.3),
                        y: 700.0 - row as f32 * 15.0,
                        width: 40.0,
                        font_size: 10.0,
                        height: 7.0,
                        font: String::new(),
                        is_bold: false,
                        is_italic: false,
                        item_type: ItemType::Text,
                        mcid: None,
                        page: 1,
                    },
                ));
            }
        }
        let refs: Vec<(usize, &TextItem)> = items.iter().map(|(i, t)| (*i, t)).collect();
        let cols = find_column_boundaries(&refs, TableDetectionMode::BodyFont);
        assert_eq!(cols.len(), 4, "Expected 4 columns, got {}", cols.len());
    }
}
