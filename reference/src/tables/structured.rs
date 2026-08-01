//! Structure-recovery-aware (TSR) table assembly.
//!
//! Consumes the raw output of an external table-structure recognition model
//! (e.g. SLANet on PaddleOCR): a flat list of HTML structure tokens plus a
//! parallel list of per-cell bboxes. Pairs each cell open-tag with its bbox
//! in document order, tracks row/column position with rowspan/colspan
//! awareness, and emits a markdown pipe-table.
//!
//! No real HTML parser is needed — the token grammar is restricted (see
//! [`parse_structure`]), so a small state machine is enough.
//!
//! Cell text is supplied separately by the caller (typically by overlap-
//! testing PDF text items against each cell's page-PDF-pt bbox).

use std::collections::{HashMap, HashSet};

/// A single resolved cell, with both structural metadata and its bbox in
/// page PDF-points (top-left origin).
#[derive(Debug, Clone)]
pub struct StructuredCell {
    /// 0-indexed grid row.
    pub row: usize,
    /// 0-indexed grid column.
    pub col: usize,
    /// 1 for a normal cell.
    pub rowspan: usize,
    /// 1 for a normal cell.
    pub colspan: usize,
    /// `true` when the cell is a `<th>` or sits inside `<thead>`.
    pub is_header: bool,
    /// Cell text (filled in by the caller after overlap-testing PDF items).
    pub text: String,
    /// Axis-aligned bbox `[x1, y1, x2, y2]` in page PDF-points, top-left origin.
    pub page_pt_bbox: [f32; 4],
}

/// Intermediate parse result before the caller fills in text + page coords.
#[derive(Debug, Clone)]
pub(crate) struct CellSlot {
    pub row: usize,
    pub col: usize,
    pub rowspan: usize,
    pub colspan: usize,
    pub is_header: bool,
    /// Index into the parallel `cell_bboxes` array.
    pub bbox_idx: usize,
}

/// Parse a sequence of SLANet structure tokens into ordered cell slots.
///
/// Token grammar (no real HTML parsing required):
/// - Section markers: `<thead>`, `</thead>`, `<tbody>`, `</tbody>` and
///   wrapper tokens (`<html>`, `<body>`, `<table>`, plus closing variants)
///   are tracked or skipped.
/// - Row markers: `<tr>` opens a new row, `</tr>` is informational.
/// - Empty cell, single token: `<td></td>` or `<th></th>`.
/// - Cell with attributes, multi-token sequence: `<td` (or `<th`), then
///   attribute fragments like ` colspan="4"`, then `>`, then later `</td>`
///   (or `</th>`). Cells get paired with the next bbox in document order.
///
/// Cells inside `<thead>` and any `<th>` cells are flagged as headers.
/// rowspan/colspan attributes are honoured and prior-row rowspans push
/// later-row cells to the right.
pub(crate) fn parse_structure(tokens: &[String]) -> Vec<CellSlot> {
    let mut slots: Vec<CellSlot> = Vec::new();
    let mut occupied: HashSet<(usize, usize)> = HashSet::new();
    let mut row: usize = 0;
    let mut col: usize = 0;
    let mut bbox_idx: usize = 0;
    let mut in_thead = false;
    let mut started_first_row = false;

    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i].trim();
        match tok {
            "<thead>" => {
                in_thead = true;
            }
            "</thead>" => {
                in_thead = false;
            }
            "<tr>" => {
                if started_first_row {
                    row += 1;
                }
                col = 0;
                started_first_row = true;
            }
            "<td></td>" | "<th></th>" => {
                let is_th = tok == "<th></th>";
                while occupied.contains(&(row, col)) {
                    col += 1;
                }
                slots.push(CellSlot {
                    row,
                    col,
                    rowspan: 1,
                    colspan: 1,
                    is_header: in_thead || is_th,
                    bbox_idx,
                });
                bbox_idx += 1;
                col += 1;
            }
            "<td" | "<th" => {
                let is_th = tok == "<th";
                let mut rowspan: usize = 1;
                let mut colspan: usize = 1;
                // Consume attribute fragments until we hit ">".
                i += 1;
                while i < tokens.len() && tokens[i].trim() != ">" {
                    let attr = tokens[i].as_str();
                    if let Some(v) = parse_int_attr(attr, "rowspan") {
                        rowspan = v.max(1);
                    } else if let Some(v) = parse_int_attr(attr, "colspan") {
                        colspan = v.max(1);
                    }
                    i += 1;
                }
                // i now points at the `>` token (or off the end if malformed).
                while occupied.contains(&(row, col)) {
                    col += 1;
                }
                slots.push(CellSlot {
                    row,
                    col,
                    rowspan,
                    colspan,
                    is_header: in_thead || is_th,
                    bbox_idx,
                });
                for r in row..row + rowspan {
                    for c in col..col + colspan {
                        occupied.insert((r, c));
                    }
                }
                bbox_idx += 1;
                col += colspan;
            }
            // Wrapper / informational tokens — no-op.
            _ => {}
        }
        i += 1;
    }

    slots
}

/// Parse an attribute fragment like ` colspan="4"` or `rowspan='2'`.
///
/// Tolerates leading whitespace and either single or double quotes.
fn parse_int_attr(s: &str, name: &str) -> Option<usize> {
    let trimmed = s.trim();
    if !trimmed.starts_with(name) {
        return None;
    }
    let rest = trimmed[name.len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let value = rest
        .trim_start_matches(['"', '\''])
        .trim_end_matches(['"', '\'']);
    value.parse().ok()
}

/// Convert a SLANet polygon (4 or 8 elements) into an axis-aligned
/// `[x1, y1, x2, y2]` rect.
///
/// 8-element form: `[x1,y1, x2,y1, x2,y2, x1,y2]` (4 corners). We ignore the
/// implicit corner order and just take min/max so rotated polygons collapse
/// to a sane bounding box.
///
/// 4-element form: `[x1, y1, x2, y2]` (axis-aligned, older SLANet variants).
pub(crate) fn polygon_to_aabb(coords: &[f32]) -> Option<[f32; 4]> {
    match coords.len() {
        4 => {
            let x1 = coords[0].min(coords[2]);
            let y1 = coords[1].min(coords[3]);
            let x2 = coords[0].max(coords[2]);
            let y2 = coords[1].max(coords[3]);
            Some([x1, y1, x2, y2])
        }
        8 => {
            let xs = [coords[0], coords[2], coords[4], coords[6]];
            let ys = [coords[1], coords[3], coords[5], coords[7]];
            let x1 = xs.iter().copied().fold(f32::INFINITY, f32::min);
            let y1 = ys.iter().copied().fold(f32::INFINITY, f32::min);
            let x2 = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let y2 = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            if x1.is_finite() && y1.is_finite() && x2.is_finite() && y2.is_finite() {
                Some([x1, y1, x2, y2])
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Convert a cell rect from crop image-pixel space to page PDF-points
/// (top-left origin), given the crop's PDF-point offset on the page and the
/// DPI the crop image was rendered at.
pub(crate) fn cell_px_to_page_pt(
    cell_px: [f32; 4],
    render_dpi: f32,
    crop_origin_pt: [f32; 2],
) -> [f32; 4] {
    let pt_per_px = if render_dpi > 0.0 {
        72.0 / render_dpi
    } else {
        1.0
    };
    let [x_off, y_off] = crop_origin_pt;
    [
        cell_px[0] * pt_per_px + x_off,
        cell_px[1] * pt_per_px + y_off,
        cell_px[2] * pt_per_px + x_off,
        cell_px[3] * pt_per_px + y_off,
    ]
}

/// Refine TSR cell bboxes into non-overlapping row/column bands.
///
/// SLANet-style bboxes are often plausible but too tall on dense borderless
/// tables. Native PDF text assignment is more reliable when each parsed row
/// owns the band between neighboring row centers instead of the full model box.
pub(crate) fn normalize_cell_bands(cells: &mut [StructuredCell]) {
    if cells.len() < 2 {
        return;
    }

    let row_bands = derive_axis_bands(cells, Axis::Y);
    let col_bands = derive_axis_bands(cells, Axis::X);

    for cell in cells {
        let row_end = cell.row + cell.rowspan.max(1).saturating_sub(1);
        if let (Some(&(y1, _)), Some(&(_, y2))) =
            (row_bands.get(&cell.row), row_bands.get(&row_end))
        {
            let clamped_y1 = cell.page_pt_bbox[1].max(y1);
            let clamped_y2 = cell.page_pt_bbox[3].min(y2);
            if clamped_y1 < clamped_y2 {
                cell.page_pt_bbox[1] = clamped_y1;
                cell.page_pt_bbox[3] = clamped_y2;
            }
        }

        let col_end = cell.col + cell.colspan.max(1).saturating_sub(1);
        if let (Some(&(x1, _)), Some(&(_, x2))) =
            (col_bands.get(&cell.col), col_bands.get(&col_end))
        {
            let clamped_x1 = cell.page_pt_bbox[0].max(x1);
            let clamped_x2 = cell.page_pt_bbox[2].min(x2);
            if clamped_x1 < clamped_x2 {
                cell.page_pt_bbox[0] = clamped_x1;
                cell.page_pt_bbox[2] = clamped_x2;
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
}

fn derive_axis_bands(cells: &[StructuredCell], axis: Axis) -> HashMap<usize, (f32, f32)> {
    let mut by_index: HashMap<usize, Vec<(f32, f32)>> = HashMap::new();

    // Prefer non-spanning cells so colspan/rowspan boxes do not skew a single
    // column/row center. If an axis has no non-spanning examples for an index,
    // fall back to anchored cells below.
    for cell in cells {
        let span = match axis {
            Axis::X => cell.colspan.max(1),
            Axis::Y => cell.rowspan.max(1),
        };
        if span == 1 {
            let idx = match axis {
                Axis::X => cell.col,
                Axis::Y => cell.row,
            };
            by_index
                .entry(idx)
                .or_default()
                .push(axis_bounds(cell.page_pt_bbox, axis));
        }
    }

    for cell in cells {
        let idx = match axis {
            Axis::X => cell.col,
            Axis::Y => cell.row,
        };
        if !by_index.contains_key(&idx) {
            by_index
                .entry(idx)
                .or_default()
                .push(axis_bounds(cell.page_pt_bbox, axis));
        }
    }

    let mut rows: Vec<(usize, f32, f32, f32)> = by_index
        .into_iter()
        .filter_map(|(idx, bounds)| {
            let mut min_edge = f32::INFINITY;
            let mut max_edge = f32::NEG_INFINITY;
            let mut center_sum = 0.0;
            let mut count = 0usize;
            for (lo, hi) in bounds {
                if lo.is_finite() && hi.is_finite() && lo < hi {
                    min_edge = min_edge.min(lo);
                    max_edge = max_edge.max(hi);
                    center_sum += (lo + hi) * 0.5;
                    count += 1;
                }
            }
            (count > 0).then_some((idx, center_sum / count as f32, min_edge, max_edge))
        })
        .collect();

    if rows.len() < 2 {
        return rows
            .into_iter()
            .map(|(idx, _center, lo, hi)| (idx, (lo, hi)))
            .collect();
    }

    rows.sort_by_key(|(idx, _, _, _)| *idx);

    let mut bands = HashMap::new();
    for i in 0..rows.len() {
        let (idx, _center, min_edge, max_edge) = rows[i];
        let lo = if i == 0 {
            min_edge
        } else {
            (rows[i - 1].1 + rows[i].1) * 0.5
        };
        let hi = if i + 1 == rows.len() {
            max_edge
        } else {
            (rows[i].1 + rows[i + 1].1) * 0.5
        };
        if lo.is_finite() && hi.is_finite() && lo < hi {
            bands.insert(idx, (lo, hi));
        }
    }

    bands
}

fn axis_bounds(bbox: [f32; 4], axis: Axis) -> (f32, f32) {
    match axis {
        Axis::X => (bbox[0].min(bbox[2]), bbox[0].max(bbox[2])),
        Axis::Y => (bbox[1].min(bbox[3]), bbox[1].max(bbox[3])),
    }
}

/// Sanitize cell text for inclusion in a markdown pipe-table cell:
/// collapse whitespace runs, drop newlines/tabs (cells must be one line),
/// and escape pipes that would otherwise break the table.
fn sanitize_cell(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    let mut prev_space = false;
    for c in text.chars() {
        match c {
            '|' => {
                s.push_str("\\|");
                prev_space = false;
            }
            '\n' | '\r' | '\t' | ' ' => {
                if !prev_space {
                    s.push(' ');
                }
                prev_space = true;
            }
            other => {
                s.push(other);
                prev_space = false;
            }
        }
    }
    s.trim().to_string()
}

/// Render a list of explicitly-positioned cells as a markdown pipe-table.
///
/// Grid dimensions are inferred from the cells' (row, col, rowspan, colspan)
/// extents. A cell with colspan/rowspan > 1 is rendered in its top-left
/// position; the absorbed grid positions are emitted as empty cells so the
/// markdown stays a valid rectangular grid that downstream readers can
/// column-count correctly.
///
/// The separator row (`|---|...|`) is emitted after the **last** row that
/// contains a header cell (`is_header == true`). When no cells are flagged
/// as headers — e.g. the upstream TSR model didn't emit `<thead>`/`<th>` —
/// the separator falls back to "after row 0" so the output is still a
/// valid pipe-table.
pub fn cells_to_markdown(cells: &[StructuredCell]) -> String {
    if cells.is_empty() {
        return String::new();
    }
    let num_rows = cells
        .iter()
        .map(|c| c.row + c.rowspan.max(1))
        .max()
        .unwrap_or(0);
    let num_cols = cells
        .iter()
        .map(|c| c.col + c.colspan.max(1))
        .max()
        .unwrap_or(0);
    if num_rows == 0 || num_cols == 0 {
        return String::new();
    }

    // Separator goes after the last header row, falling back to row 0 when
    // no header cells exist. Clamped into range so a malformed cell with
    // row >= num_rows can't push it past the table.
    let separator_after_row = cells
        .iter()
        .filter(|c| c.is_header)
        .map(|c| c.row)
        .max()
        .unwrap_or(0)
        .min(num_rows.saturating_sub(1));

    let mut grid: Vec<Vec<String>> = vec![vec![String::new(); num_cols]; num_rows];
    for cell in cells {
        if cell.row < num_rows && cell.col < num_cols {
            grid[cell.row][cell.col] = sanitize_cell(&cell.text);
        }
    }

    let mut output = String::new();
    for (row_idx, row) in grid.iter().enumerate() {
        output.push('|');
        for cell in row {
            output.push_str(cell);
            output.push('|');
        }
        output.push('\n');
        if row_idx == separator_after_row {
            output.push('|');
            for _ in 0..num_cols {
                output.push_str("---|");
            }
            output.push('\n');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> String {
        s.to_string()
    }

    /// Tokens for the synthetic 3×3 grid example (one colspan-4 row + two
    /// data rows of 4 cells each = 9 cells total, 3 rows × 4 cols).
    fn synthetic_3x3_tokens() -> Vec<String> {
        vec![
            "<html>",
            "<body>",
            "<table>",
            "<tbody>",
            "<tr>",
            "<td",
            " colspan=\"4\"",
            ">",
            "</td>",
            "</tr>",
            "<tr>",
            "<td></td>",
            "<td></td>",
            "<td></td>",
            "<td></td>",
            "</tr>",
            "<tr>",
            "<td></td>",
            "<td></td>",
            "<td></td>",
            "<td></td>",
            "</tr>",
            "</tbody>",
            "</table>",
            "</body>",
            "</html>",
        ]
        .into_iter()
        .map(t)
        .collect()
    }

    /// Bboxes for the synthetic 3×3 grid (8-element polygon form), all
    /// within a 400×120 px crop.
    fn synthetic_3x3_bboxes() -> Vec<Vec<f32>> {
        vec![
            vec![3.0, 2.0, 395.0, 2.0, 396.0, 59.0, 3.0, 59.0],
            vec![26.0, 62.0, 140.0, 62.0, 141.0, 120.0, 26.0, 120.0],
            vec![149.0, 64.0, 248.0, 64.0, 248.0, 119.0, 149.0, 119.0],
            vec![257.0, 64.0, 350.0, 64.0, 350.0, 119.0, 257.0, 119.0],
            vec![359.0, 64.0, 395.0, 64.0, 395.0, 119.0, 359.0, 119.0],
            vec![26.0, 122.0, 140.0, 122.0, 140.0, 178.0, 26.0, 178.0],
            vec![149.0, 124.0, 248.0, 124.0, 248.0, 179.0, 149.0, 179.0],
            vec![257.0, 124.0, 350.0, 124.0, 350.0, 179.0, 257.0, 179.0],
            vec![359.0, 124.0, 395.0, 124.0, 395.0, 179.0, 359.0, 179.0],
        ]
    }

    #[test]
    fn parse_structure_synthetic_3x3() {
        let tokens = synthetic_3x3_tokens();
        let slots = parse_structure(&tokens);

        assert_eq!(slots.len(), 9, "should parse 9 cells");

        // Cell 0: row 0 col 0, colspan 4
        assert_eq!(slots[0].row, 0);
        assert_eq!(slots[0].col, 0);
        assert_eq!(slots[0].colspan, 4);
        assert_eq!(slots[0].rowspan, 1);

        // Cells 1..5: row 1, cols 0..3
        for (i, slot) in slots.iter().enumerate().skip(1).take(4) {
            assert_eq!(slot.row, 1, "cell {i}: row should be 1");
            assert_eq!(slot.col, i - 1, "cell {i}: col should be {}", i - 1);
            assert_eq!(slot.colspan, 1);
            assert_eq!(slot.rowspan, 1);
        }

        // Cells 5..9: row 2, cols 0..3
        for (i, slot) in slots.iter().enumerate().skip(5).take(4) {
            assert_eq!(slot.row, 2, "cell {i}: row should be 2");
            assert_eq!(slot.col, i - 5);
            assert_eq!(slot.colspan, 1);
        }
    }

    #[test]
    fn polygon_to_aabb_8elt() {
        // Synthetic cell bbox 0
        let coords = vec![3.0, 2.0, 395.0, 2.0, 396.0, 59.0, 3.0, 59.0];
        let aabb = polygon_to_aabb(&coords).unwrap();
        assert_eq!(aabb, [3.0, 2.0, 396.0, 59.0]);
    }

    #[test]
    fn polygon_to_aabb_4elt() {
        let coords = vec![5.0, 10.0, 50.0, 60.0];
        let aabb = polygon_to_aabb(&coords).unwrap();
        assert_eq!(aabb, [5.0, 10.0, 50.0, 60.0]);
    }

    #[test]
    fn polygon_to_aabb_4elt_unordered() {
        // Caller may pass corners in any order; min/max should normalise.
        let coords = vec![50.0, 60.0, 5.0, 10.0];
        let aabb = polygon_to_aabb(&coords).unwrap();
        assert_eq!(aabb, [5.0, 10.0, 50.0, 60.0]);
    }

    #[test]
    fn polygon_to_aabb_invalid_len() {
        assert!(polygon_to_aabb(&[1.0, 2.0, 3.0]).is_none());
        assert!(polygon_to_aabb(&[1.0; 6]).is_none());
        assert!(polygon_to_aabb(&[]).is_none());
    }

    #[test]
    fn synthetic_3x3_aabbs_inside_crop() {
        // All 9 bboxes should produce valid (x1<x2, y1<y2) rects within the
        // crop bounds (400 wide, ~180 tall by inspection of the fixture).
        let bboxes = synthetic_3x3_bboxes();
        assert_eq!(bboxes.len(), 9);
        for (i, bb) in bboxes.iter().enumerate() {
            let aabb = polygon_to_aabb(bb).unwrap_or_else(|| panic!("bbox {i} invalid"));
            assert!(aabb[0] < aabb[2], "bbox {i}: x1 < x2");
            assert!(aabb[1] < aabb[3], "bbox {i}: y1 < y2");
            assert!(aabb[0] >= 0.0 && aabb[2] <= 500.0, "bbox {i}: within crop");
            assert!(aabb[1] >= 0.0 && aabb[3] <= 200.0, "bbox {i}: within crop");
        }
    }

    #[test]
    fn normalize_cell_bands_splits_overlapping_slanet_rows() {
        let mut cells = vec![
            StructuredCell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: true,
                text: String::new(),
                page_pt_bbox: [10.0, 100.0, 90.0, 120.0],
            },
            StructuredCell {
                row: 0,
                col: 1,
                rowspan: 1,
                colspan: 1,
                is_header: true,
                text: String::new(),
                page_pt_bbox: [90.0, 100.0, 170.0, 120.0],
            },
            StructuredCell {
                row: 1,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [10.0, 116.0, 90.0, 136.0],
            },
            StructuredCell {
                row: 1,
                col: 1,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [90.0, 116.0, 170.0, 136.0],
            },
        ];

        normalize_cell_bands(&mut cells);

        assert_eq!(cells[0].page_pt_bbox[3], cells[2].page_pt_bbox[1]);
        assert_eq!(cells[1].page_pt_bbox[3], cells[3].page_pt_bbox[1]);
        assert!(
            (cells[0].page_pt_bbox[3] - 118.0).abs() < 0.01,
            "row separator should be midpoint between row centers: {:?}",
            cells
        );
    }

    #[test]
    fn normalize_cell_bands_preserves_colspan_extent() {
        let mut cells = vec![
            StructuredCell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 2,
                is_header: true,
                text: String::new(),
                page_pt_bbox: [8.0, 80.0, 172.0, 98.0],
            },
            StructuredCell {
                row: 1,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [10.0, 96.0, 90.0, 114.0],
            },
            StructuredCell {
                row: 1,
                col: 1,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: String::new(),
                page_pt_bbox: [88.0, 96.0, 170.0, 114.0],
            },
        ];

        normalize_cell_bands(&mut cells);

        assert!(
            cells[0].page_pt_bbox[0] <= cells[1].page_pt_bbox[0],
            "spanning cell should retain the first column's left edge"
        );
        assert!(
            cells[0].page_pt_bbox[2] >= cells[2].page_pt_bbox[2],
            "spanning cell should retain the last column's right edge"
        );
    }

    #[test]
    fn parse_int_attr_basic() {
        assert_eq!(parse_int_attr(" colspan=\"4\"", "colspan"), Some(4));
        assert_eq!(parse_int_attr(" rowspan=\"2\"", "rowspan"), Some(2));
        assert_eq!(parse_int_attr("colspan='3'", "colspan"), Some(3));
        assert_eq!(parse_int_attr(" colspan=\"4\"", "rowspan"), None);
        assert_eq!(parse_int_attr(" class=\"foo\"", "colspan"), None);
    }

    #[test]
    fn parse_structure_rowspan_pushes_next_row_right() {
        // <tr><td rowspan="2">A</td><td>B</td></tr><tr><td>C</td></tr>
        // Expected: A at (0,0), B at (0,1), C at (1,1) — col 0 of row 1
        // is occupied by A's rowspan.
        let tokens: Vec<String> = vec![
            "<table>",
            "<tbody>",
            "<tr>",
            "<td",
            " rowspan=\"2\"",
            ">",
            "</td>",
            "<td></td>",
            "</tr>",
            "<tr>",
            "<td></td>",
            "</tr>",
            "</tbody>",
            "</table>",
        ]
        .into_iter()
        .map(t)
        .collect();

        let slots = parse_structure(&tokens);
        assert_eq!(slots.len(), 3);
        assert_eq!((slots[0].row, slots[0].col), (0, 0));
        assert_eq!(slots[0].rowspan, 2);
        assert_eq!((slots[1].row, slots[1].col), (0, 1));
        // C should be at (1, 1) because (1, 0) is occupied by A's rowspan.
        assert_eq!((slots[2].row, slots[2].col), (1, 1));
    }

    #[test]
    fn parse_structure_thead_marks_headers() {
        // <thead><tr><th>H1</th><th>H2</th></tr></thead>
        // <tbody><tr><td>D1</td><td>D2</td></tr></tbody>
        let tokens: Vec<String> = vec![
            "<table>",
            "<thead>",
            "<tr>",
            "<th></th>",
            "<th></th>",
            "</tr>",
            "</thead>",
            "<tbody>",
            "<tr>",
            "<td></td>",
            "<td></td>",
            "</tr>",
            "</tbody>",
            "</table>",
        ]
        .into_iter()
        .map(t)
        .collect();

        let slots = parse_structure(&tokens);
        assert_eq!(slots.len(), 4);
        assert!(slots[0].is_header && slots[1].is_header);
        assert!(!slots[2].is_header && !slots[3].is_header);
    }

    #[test]
    fn parse_structure_th_outside_thead_still_header() {
        // A row-header style: leading <th> in tbody.
        let tokens: Vec<String> = vec![
            "<table>",
            "<tbody>",
            "<tr>",
            "<th></th>",
            "<td></td>",
            "</tr>",
            "</tbody>",
            "</table>",
        ]
        .into_iter()
        .map(t)
        .collect();

        let slots = parse_structure(&tokens);
        assert_eq!(slots.len(), 2);
        assert!(slots[0].is_header);
        assert!(!slots[1].is_header);
    }

    #[test]
    fn parse_structure_th_with_attrs() {
        let tokens: Vec<String> = vec![
            "<table>",
            "<thead>",
            "<tr>",
            "<th",
            " colspan=\"2\"",
            ">",
            "</th>",
            "</tr>",
            "</thead>",
            "</table>",
        ]
        .into_iter()
        .map(t)
        .collect();
        let slots = parse_structure(&tokens);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].colspan, 2);
        assert!(slots[0].is_header);
    }

    #[test]
    fn cells_to_markdown_synthetic_3x3() {
        // Build the cells the parser would produce for the synthetic grid,
        // and provide some sample text so we can sanity-check output.
        let cells = vec![
            StructuredCell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 4,
                is_header: false,
                text: "Title".into(),
                page_pt_bbox: [0.0, 0.0, 0.0, 0.0],
            },
            StructuredCell {
                row: 1,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: "a".into(),
                page_pt_bbox: [0.0, 0.0, 0.0, 0.0],
            },
            StructuredCell {
                row: 1,
                col: 1,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: "b".into(),
                page_pt_bbox: [0.0, 0.0, 0.0, 0.0],
            },
            StructuredCell {
                row: 1,
                col: 2,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: "c".into(),
                page_pt_bbox: [0.0, 0.0, 0.0, 0.0],
            },
            StructuredCell {
                row: 1,
                col: 3,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: "d".into(),
                page_pt_bbox: [0.0, 0.0, 0.0, 0.0],
            },
        ];

        let md = cells_to_markdown(&cells);
        // Header row contains the spanning cell text in col 0 and pads to 4 cols.
        // Absorbed-by-colspan positions render as empty cells (no padding).
        assert!(md.starts_with("|Title||||\n"), "got: {md}");
        assert!(md.contains("|---|---|---|---|\n"));
        assert!(md.contains("|a|b|c|d|\n"));
    }

    #[test]
    fn cells_to_markdown_escapes_pipes() {
        let cells = vec![
            StructuredCell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: "a|b".into(),
                page_pt_bbox: [0.0, 0.0, 0.0, 0.0],
            },
            StructuredCell {
                row: 0,
                col: 1,
                rowspan: 1,
                colspan: 1,
                is_header: false,
                text: "x".into(),
                page_pt_bbox: [0.0, 0.0, 0.0, 0.0],
            },
        ];
        let md = cells_to_markdown(&cells);
        assert!(md.contains("|a\\|b|x|"));
    }

    #[test]
    fn cells_to_markdown_collapses_whitespace_and_newlines() {
        let cells = vec![StructuredCell {
            row: 0,
            col: 0,
            rowspan: 1,
            colspan: 1,
            is_header: false,
            text: "foo  \n  bar\tbaz".into(),
            page_pt_bbox: [0.0, 0.0, 0.0, 0.0],
        }];
        let md = cells_to_markdown(&cells);
        assert!(md.contains("|foo bar baz|"));
    }

    fn cell(row: usize, col: usize, is_header: bool, text: &str) -> StructuredCell {
        StructuredCell {
            row,
            col,
            rowspan: 1,
            colspan: 1,
            is_header,
            text: text.into(),
            page_pt_bbox: [0.0, 0.0, 0.0, 0.0],
        }
    }

    #[test]
    fn cells_to_markdown_separator_after_last_header_row() {
        // Two-row header (a multi-row thead), then two body rows. Separator
        // should land after row 1 (the LAST header row), not after row 0.
        let cells = vec![
            cell(0, 0, true, "H0a"),
            cell(0, 1, true, "H0b"),
            cell(1, 0, true, "H1a"),
            cell(1, 1, true, "H1b"),
            cell(2, 0, false, "d0a"),
            cell(2, 1, false, "d0b"),
            cell(3, 0, false, "d1a"),
            cell(3, 1, false, "d1b"),
        ];
        let md = cells_to_markdown(&cells);
        let expected = "|H0a|H0b|\n|H1a|H1b|\n|---|---|\n|d0a|d0b|\n|d1a|d1b|\n";
        assert_eq!(md, expected, "got: {md}");
    }

    #[test]
    fn cells_to_markdown_separator_when_row_0_not_header() {
        // Row 0 is not flagged as a header but row 1 is. Separator should
        // follow row 1 (the header), demonstrating that we don't blindly
        // emit after row 0.
        let cells = vec![
            cell(0, 0, false, "x0a"),
            cell(0, 1, false, "x0b"),
            cell(1, 0, true, "Hdr1"),
            cell(1, 1, true, "Hdr2"),
            cell(2, 0, false, "data1"),
            cell(2, 1, false, "data2"),
        ];
        let md = cells_to_markdown(&cells);
        // Confirm the separator is NOT after row 0.
        assert!(!md.starts_with("|x0a|x0b|\n|---|"), "got: {md}");
        // Confirm it IS after row 1.
        assert!(
            md.contains("|Hdr1|Hdr2|\n|---|---|\n|data1|data2|"),
            "got: {md}"
        );
    }

    #[test]
    fn cells_to_markdown_no_headers_falls_back_to_row_0() {
        // No header cells at all — fallback: separator after row 0 so the
        // output is still a valid markdown pipe-table.
        let cells = vec![
            cell(0, 0, false, "a"),
            cell(0, 1, false, "b"),
            cell(1, 0, false, "c"),
            cell(1, 1, false, "d"),
        ];
        let md = cells_to_markdown(&cells);
        assert_eq!(md, "|a|b|\n|---|---|\n|c|d|\n");
    }
}
