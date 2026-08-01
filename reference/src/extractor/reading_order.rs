//! Region-graph evidence for page reading order.
//!
//! Whole-page column histograms fail when images or spanning captions occupy
//! only part of a page. This module turns image geometry and repeated row
//! gutters into a small directed acyclic graph: content above a local column
//! band, the left flow, the right flow, and content below it. The graph is
//! deliberately evidence-gated; ordinary pages keep the established layout
//! path.

use crate::text_utils::{effective_width, is_cjk_char, is_rtl_text};
use crate::types::TextItem;

const MIN_IMAGE_WIDTH: f32 = 60.0;
const MIN_IMAGE_HEIGHT: f32 = 40.0;
const MIN_ROW_GUTTER: f32 = 8.0;
const SPLIT_CLUSTER_TOLERANCE: f32 = 20.0;
const MIN_ALIGNED_ROWS: usize = 4;

pub(crate) type ImageRegion = (f32, f32, f32, f32);

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ColumnFlowBand {
    pub(crate) split_x: f32,
    pub(crate) y_bottom: f32,
    pub(crate) y_top: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegionKind {
    FullWidth,
    Column,
}

#[derive(Debug)]
pub(crate) struct RegionNode {
    pub(crate) kind: RegionKind,
    pub(crate) items: Vec<TextItem>,
}

#[derive(Debug)]
struct Row<'a> {
    y: f32,
    items: Vec<&'a TextItem>,
}

fn page_x_bounds(items: &[TextItem], images: &[ImageRegion]) -> Option<(f32, f32)> {
    let text_min = items
        .iter()
        .map(|item| item.x)
        .fold(f32::INFINITY, f32::min);
    let text_max = items
        .iter()
        .map(|item| item.x + effective_width(item))
        .fold(f32::NEG_INFINITY, f32::max);
    let image_min = images
        .iter()
        .map(|region| region.0.min(region.2))
        .fold(f32::INFINITY, f32::min);
    let image_max = images
        .iter()
        .map(|region| region.0.max(region.2))
        .fold(f32::NEG_INFINITY, f32::max);
    let x_min = text_min.min(image_min);
    let x_max = text_max.max(image_max);
    (x_min.is_finite() && x_max.is_finite() && x_max > x_min).then_some((x_min, x_max))
}

fn group_rows(items: &[TextItem]) -> Vec<Row<'_>> {
    const Y_TOLERANCE: f32 = 3.0;
    let mut sorted: Vec<&TextItem> = items.iter().collect();
    sorted.sort_by(|left, right| right.y.total_cmp(&left.y));
    let mut rows: Vec<Row<'_>> = Vec::new();
    for item in sorted {
        if let Some(row) = rows
            .last_mut()
            .filter(|row| (row.y - item.y).abs() <= Y_TOLERANCE)
        {
            row.items.push(item);
            row.y = row.items.iter().map(|member| member.y).sum::<f32>() / row.items.len() as f32;
        } else {
            rows.push(Row {
                y: item.y,
                items: vec![item],
            });
        }
    }
    for row in &mut rows {
        row.items.sort_by(|left, right| left.x.total_cmp(&right.x));
    }
    rows
}

fn side_is_prose(items: &[&TextItem]) -> bool {
    let text = items
        .iter()
        .map(|item| item.text.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let alphabetic_count = text
        .chars()
        .filter(|character| character.is_alphabetic())
        .count();
    let cjk_count = text
        .chars()
        .filter(|character| is_cjk_char(*character))
        .count();
    (text.split_whitespace().count() >= 3 || cjk_count >= 10) && alphabetic_count >= 10
}

fn aligned_row_split(row: &Row<'_>, x_min: f32, x_max: f32) -> Option<f32> {
    if row.items.len() < 2 {
        return None;
    }
    let page_width = x_max - x_min;
    let center_low = x_min + page_width * 0.25;
    let center_high = x_min + page_width * 0.75;
    row.items
        .windows(2)
        .filter_map(|pair| {
            let left_end = pair[0].x + effective_width(pair[0]);
            let right_start = pair[1].x;
            let gap = right_start - left_end;
            let split_x = (left_end + right_start) / 2.0;
            if gap < MIN_ROW_GUTTER || split_x < center_low || split_x > center_high {
                return None;
            }
            let left: Vec<&TextItem> = row
                .items
                .iter()
                .copied()
                .filter(|item| item.x + effective_width(item) / 2.0 < split_x)
                .collect();
            let right: Vec<&TextItem> = row
                .items
                .iter()
                .copied()
                .filter(|item| item.x + effective_width(item) / 2.0 >= split_x)
                .collect();
            (side_is_prose(&left) && side_is_prose(&right)).then_some((split_x, gap))
        })
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .map(|candidate| candidate.0)
}

fn local_flow_below_full_width_image(
    items: &[TextItem],
    images: &[ImageRegion],
    x_min: f32,
    x_max: f32,
) -> Option<ColumnFlowBand> {
    let page_width = x_max - x_min;
    let full_width_images: Vec<ImageRegion> = images
        .iter()
        .copied()
        .filter(|&(x0, y0, x1, y1)| {
            let width = (x1 - x0).abs();
            let height = (y1 - y0).abs();
            width >= page_width * 0.65 && height >= 60.0
        })
        .collect();
    // A local column flow below an image is only unambiguous for a single,
    // nearly square hero/figure. Wide report banners and full-page artwork
    // frequently sit above unrelated page furniture whose aligned labels can
    // mimic prose columns.
    if full_width_images.len() != 1 {
        return None;
    }
    let (image_x0, _, image_x1, _) = full_width_images[0];
    let anchor_width = (image_x1 - image_x0).abs();
    let anchor_height = (full_width_images[0].3 - full_width_images[0].1).abs();
    if anchor_width < page_width * 0.85
        || anchor_height < anchor_width * 0.85
        || anchor_height > anchor_width * 1.2
    {
        return None;
    }
    let image_bottom = full_width_images
        .iter()
        .map(|&(_, y0, _, y1)| y0.min(y1))
        .fold(f32::NEG_INFINITY, f32::max);
    if !image_bottom.is_finite() {
        return None;
    }

    let below: Vec<TextItem> = items
        .iter()
        .filter(|item| item.y < image_bottom && item.y >= image_bottom - 220.0)
        .cloned()
        .collect();
    let candidates: Vec<(f32, f32)> = group_rows(&below)
        .into_iter()
        .filter_map(|row| aligned_row_split(&row, x_min, x_max).map(|split| (split, row.y)))
        .collect();
    if candidates.len() < MIN_ALIGNED_ROWS {
        return None;
    }

    let mut clusters: Vec<Vec<(f32, f32)>> = Vec::new();
    for candidate in candidates {
        if let Some(cluster) = clusters.iter_mut().find(|cluster| {
            let mean = cluster.iter().map(|entry| entry.0).sum::<f32>() / cluster.len() as f32;
            (mean - candidate.0).abs() <= SPLIT_CLUSTER_TOLERANCE
        }) {
            cluster.push(candidate);
        } else {
            clusters.push(vec![candidate]);
        }
    }
    let dominant = clusters.into_iter().max_by_key(Vec::len)?;
    if dominant.len() < MIN_ALIGNED_ROWS {
        return None;
    }
    let split_x = dominant.iter().map(|entry| entry.0).sum::<f32>() / dominant.len() as f32;
    let y_top = dominant
        .iter()
        .map(|entry| entry.1)
        .fold(f32::NEG_INFINITY, f32::max)
        + 3.0;
    let image_gap = image_bottom - y_top;
    if !(60.0..=120.0).contains(&image_gap) {
        return None;
    }
    let y_bottom = dominant
        .iter()
        .map(|entry| entry.1)
        .fold(f32::INFINITY, f32::min)
        - 3.0;
    if y_top - y_bottom > 130.0 {
        return None;
    }
    log::debug!(
        "page {}: full-width image flow images={} aligned_rows={} split={:.1} page=[{:.1}..{:.1}] image_bottom={:.1} y=[{:.1}..{:.1}] full_width={:?}",
        items.first().map_or(0, |item| item.page),
        images.len(),
        dominant.len(),
        split_x,
        x_min,
        x_max,
        image_bottom,
        y_bottom,
        y_top,
        full_width_images
    );
    Some(ColumnFlowBand {
        split_x,
        y_bottom,
        y_top,
    })
}

fn paired_column_images(
    items: &[TextItem],
    images: &[ImageRegion],
    split_x: f32,
    x_min: f32,
    x_max: f32,
) -> Option<ColumnFlowBand> {
    let page_width = x_max - x_min;
    if split_x < x_min + page_width * 0.4 || split_x > x_min + page_width * 0.6 {
        return None;
    }
    let qualifying: Vec<ImageRegion> = images
        .iter()
        .copied()
        .filter(|&(x0, y0, x1, y1)| {
            let image_left = x0.min(x1);
            let image_right = x0.max(x1);
            let confined_to_one_column = image_right <= split_x || image_left >= split_x;
            confined_to_one_column
                && (x1 - x0).abs() >= MIN_IMAGE_WIDTH
                && (y1 - y0).abs() >= MIN_IMAGE_HEIGHT
        })
        .collect();
    let wide_images: Vec<ImageRegion> = qualifying
        .iter()
        .copied()
        .filter(|(x0, _, x1, _)| (x1 - x0).abs() >= page_width * 0.35)
        .collect();
    if qualifying.len() < 3 || wide_images.len() < 3 {
        return None;
    }
    let has_left = qualifying
        .iter()
        .any(|&(x0, _, x1, _)| (x0 + x1) / 2.0 < split_x);
    let has_right = qualifying
        .iter()
        .any(|&(x0, _, x1, _)| (x0 + x1) / 2.0 >= split_x);
    if !has_left || !has_right {
        return None;
    }
    // A meaningful image-backed column flow spans multiple vertical panels.
    // Three same-row header/logo images can otherwise satisfy the image count
    // and send an ordinary asymmetric page through sequential column order.
    let image_y_min = wide_images
        .iter()
        .map(|region| region.1.min(region.3))
        .fold(f32::INFINITY, f32::min);
    let image_y_max = wide_images
        .iter()
        .map(|region| region.1.max(region.3))
        .fold(f32::NEG_INFINITY, f32::max);
    let has_vertical_stack = wide_images.iter().enumerate().any(|(index, left)| {
        wide_images.iter().skip(index + 1).any(|right| {
            let same_side =
                ((left.0 + left.2) / 2.0 < split_x) == ((right.0 + right.2) / 2.0 < split_x);
            let left_center = (left.1 + left.3) / 2.0;
            let right_center = (right.1 + right.3) / 2.0;
            let left_height = (left.3 - left.1).abs();
            let right_height = (right.3 - right.1).abs();
            let vertical_gap = if left.1.max(left.3) < right.1.min(right.3) {
                right.1.min(right.3) - left.1.max(left.3)
            } else if right.1.max(right.3) < left.1.min(left.3) {
                left.1.min(left.3) - right.1.max(right.3)
            } else {
                0.0
            };
            same_side
                && (left_center - right_center).abs() >= left_height.min(right_height) * 0.5
                && vertical_gap <= left_height.max(right_height) * 0.5
        })
    });
    if image_y_max - image_y_min < page_width * 0.45 || !has_vertical_stack {
        return None;
    }
    let y_top = qualifying
        .iter()
        .map(|region| region.1.max(region.3))
        .fold(f32::NEG_INFINITY, f32::max)
        + 3.0;
    // Only column-confined text proves the lower extent of the flow. A
    // spanning heading or caption below the columns must become the trailing
    // full-width node rather than stretching the column band to the page foot.
    let y_bottom = items
        .iter()
        .filter(|item| {
            let item_right = item.x + effective_width(item);
            item.y <= y_top && (item_right <= split_x || item.x >= split_x)
        })
        .map(|item| item.y)
        .fold(f32::INFINITY, f32::min)
        - 3.0;
    if !y_bottom.is_finite() {
        return None;
    }
    let distinct_rows = |right: bool| {
        let mut ys: Vec<f32> = items
            .iter()
            .filter(|item| {
                item.y <= y_top && (item.x + effective_width(item) / 2.0 >= split_x) == right
            })
            .map(|item| item.y)
            .collect();
        ys.sort_by(|left, right| left.total_cmp(right));
        ys.dedup_by(|left, right| (*left - *right).abs() <= 3.0);
        ys.len()
    };
    let left_rows = distinct_rows(false);
    let right_rows = distinct_rows(true);
    let line_balance = left_rows.min(right_rows) as f32 / left_rows.max(right_rows).max(1) as f32;
    (left_rows >= 5 && right_rows >= 5 && line_balance < 0.55).then(|| {
        log::debug!(
            "page {}: paired-image flow qualifying_images={} rows={}/{} split={:.1} page=[{:.1}..{:.1}] y=[{:.1}..{:.1}] images={:?}",
            items.first().map_or(0, |item| item.page),
            qualifying.len(),
            left_rows,
            right_rows,
            split_x,
            x_min,
            x_max,
            y_bottom,
            y_top,
            qualifying
        );
        ColumnFlowBand {
            split_x,
            y_bottom,
            y_top,
        }
    })
}

pub(crate) fn infer_image_anchored_flow(
    items: &[TextItem],
    images: &[ImageRegion],
    detected_split: Option<f32>,
) -> Option<ColumnFlowBand> {
    if items.is_empty() || images.is_empty() {
        return None;
    }
    let (x_min, x_max) = page_x_bounds(items, images)?;
    detected_split
        .and_then(|split_x| paired_column_images(items, images, split_x, x_min, x_max))
        .or_else(|| local_flow_below_full_width_image(items, images, x_min, x_max))
}

/// Partition a page into the topological order `above -> left -> right -> below`.
/// These edges encode the reading-order DAG; empty nodes are omitted.
pub(crate) fn build_region_graph(items: Vec<TextItem>, band: ColumnFlowBand) -> Vec<RegionNode> {
    let mut above = Vec::new();
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut below = Vec::new();
    for item in items {
        if item.y > band.y_top {
            above.push(item);
        } else if item.y < band.y_bottom {
            below.push(item);
        } else if item.x + effective_width(&item) / 2.0 < band.split_x {
            left.push(item);
        } else {
            right.push(item);
        }
    }
    let rtl = is_rtl_text(left.iter().chain(right.iter()).map(|item| &item.text));
    let mut ordered = vec![(RegionKind::FullWidth, above)];
    if rtl {
        ordered.push((RegionKind::Column, right));
        ordered.push((RegionKind::Column, left));
    } else {
        ordered.push((RegionKind::Column, left));
        ordered.push((RegionKind::Column, right));
    }
    ordered.push((RegionKind::FullWidth, below));
    ordered
        .into_iter()
        .filter_map(|(kind, items)| (!items.is_empty()).then_some(RegionNode { kind, items }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    fn item(text: &str, x: f32, y: f32, width: f32) -> TextItem {
        TextItem {
            text: text.into(),
            x,
            y,
            width,
            height: 11.0,
            font: "F1".into(),
            font_size: 11.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn full_width_image_anchors_local_two_column_flow() {
        let mut items = vec![
            item("A full width caption", 55.0, 230.0, 430.0),
            item("A trailing full width heading", 55.0, 80.0, 430.0),
        ];
        for index in 0..5 {
            let y = 170.0 - index as f32 * 14.0;
            items.push(item("left column prose words", 55.0, y, 210.0));
            items.push(item("right column prose words", 280.0, y, 210.0));
        }
        let images = vec![(55.0, 250.0, 490.0, 680.0)];
        let band = infer_image_anchored_flow(&items, &images, None).unwrap();
        assert!((band.split_x - 272.5).abs() < 2.0);
        let graph = build_region_graph(items, band);
        assert_eq!(graph.len(), 4);
        assert_eq!(graph[0].kind, RegionKind::FullWidth);
        assert_eq!(graph[1].kind, RegionKind::Column);
        assert_eq!(graph[2].kind, RegionKind::Column);
        assert_eq!(graph[3].kind, RegionKind::FullWidth);
        assert_eq!(graph[3].items[0].text, "A trailing full width heading");
    }

    #[test]
    fn full_width_image_anchors_cjk_column_flow() {
        let mut items = Vec::new();
        for index in 0..5 {
            let y = 170.0 - index as f32 * 14.0;
            items.push(item("左栏这是没有空格的正文内容", 55.0, y, 210.0));
            items.push(item("右栏这是没有空格的正文内容", 280.0, y, 210.0));
        }
        let images = vec![(55.0, 250.0, 490.0, 680.0)];

        assert!(infer_image_anchored_flow(&items, &images, None).is_some());
    }

    #[test]
    fn paired_images_anchor_unbalanced_column_flows() {
        let mut items = vec![
            item("running header", 55.0, 700.0, 430.0),
            item("trailing full width caption", 55.0, 300.0, 430.0),
        ];
        for index in 0..5 {
            items.push(item(
                "left prose words",
                55.0,
                500.0 - index as f32 * 14.0,
                200.0,
            ));
            items.push(item(
                "right prose words",
                280.0,
                520.0 - index as f32 * 14.0,
                200.0,
            ));
        }
        for index in 5..12 {
            items.push(item(
                "right continuation prose words",
                280.0,
                520.0 - index as f32 * 14.0,
                200.0,
            ));
        }
        let images = vec![
            (55.0, 530.0, 255.0, 680.0),
            (55.0, 380.0, 255.0, 530.0),
            (280.0, 560.0, 490.0, 680.0),
        ];
        let band = infer_image_anchored_flow(&items, &images, Some(270.0)).unwrap();
        let graph = build_region_graph(items, band);
        assert_eq!(graph[0].kind, RegionKind::FullWidth);
        assert_eq!(graph[1].kind, RegionKind::Column);
        assert_eq!(graph[2].kind, RegionKind::Column);
        assert_eq!(graph[3].kind, RegionKind::FullWidth);
        assert_eq!(graph[3].items[0].text, "trailing full width caption");
    }

    #[test]
    fn rtl_region_graph_reads_right_column_first() {
        let items = vec![
            item("A long English report header", 55.0, 250.0, 430.0),
            item("نص العمود الأيسر", 55.0, 150.0, 180.0),
            item("نص العمود الأيمن", 300.0, 150.0, 180.0),
        ];
        let graph = build_region_graph(
            items,
            ColumnFlowBand {
                split_x: 270.0,
                y_bottom: 100.0,
                y_top: 200.0,
            },
        );

        assert_eq!(graph.len(), 3);
        assert_eq!(graph[0].kind, RegionKind::FullWidth);
        assert!(graph[1].items[0].x > graph[2].items[0].x);
    }

    #[test]
    fn paired_header_logos_do_not_anchor_page_columns() {
        let mut items = Vec::new();
        for index in 0..7 {
            items.push(item(
                "left prose words",
                55.0,
                700.0 - index as f32 * 14.0,
                200.0,
            ));
        }
        for index in 0..30 {
            items.push(item(
                "right prose words",
                280.0,
                700.0 - index as f32 * 14.0,
                200.0,
            ));
        }
        let images = vec![
            (55.0, 720.0, 205.0, 770.0),
            (60.0, 718.0, 210.0, 768.0),
            (280.0, 720.0, 450.0, 770.0),
        ];
        assert!(infer_image_anchored_flow(&items, &images, Some(270.0)).is_none());
    }

    #[test]
    fn wide_banner_does_not_anchor_local_columns() {
        let mut items = Vec::new();
        for index in 0..7 {
            let y = 270.0 - index as f32 * 14.0;
            items.push(item("left column prose words", 55.0, y, 210.0));
            items.push(item("right column prose words", 280.0, y, 210.0));
        }
        let images = vec![(55.0, 310.0, 490.0, 550.0)];
        assert!(infer_image_anchored_flow(&items, &images, None).is_none());
    }
}
