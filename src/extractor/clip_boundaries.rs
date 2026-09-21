//! Conservative clipping evidence: keeping independent text runs apart, and
//! leaving out the runs a clip hides altogether.
//!
//! Only a single finite, axis-aligned rectangle can establish a boundary;
//! unknown paths retain the existing merge behavior, even if a later
//! rectangle narrows their clip, and never hide anything. Nothing here
//! infers cells or trims a run to its clip: a run is left out only when it
//! lies wholly outside the rectangle (`drop_clipped_away_runs`) and is kept
//! as painted otherwise.

use super::get_number;
use crate::types::{ItemType, TextItem};
use lopdf::Object;

const TOLERANCE: f32 = 0.01;

#[derive(Clone, Copy, Debug)]
pub(super) struct ClipRect {
    left: f32,
    bottom: f32,
    right: f32,
    top: f32,
}

impl ClipRect {
    fn intersection(self, other: Self) -> Option<Self> {
        let rect = Self {
            left: self.left.max(other.left),
            bottom: self.bottom.max(other.bottom),
            right: self.right.min(other.right),
            top: self.top.min(other.top),
        };
        (rect.left < rect.right && rect.bottom < rect.top).then_some(rect)
    }

    fn from_path(operands: &[Object], ctm: [f32; 6]) -> Option<Self> {
        if operands.len() != 4
            || !ctm.iter().all(|x| x.is_finite())
            || ctm[1] != 0.0
            || ctm[2] != 0.0
        {
            return None;
        }
        let [x, y, w, h] = std::array::from_fn(|i| get_number(&operands[i]));
        let (x, y, w, h) = (x?, y?, w?, h?);
        let xs = [ctm[0] * x + ctm[4], ctm[0] * (x + w) + ctm[4]];
        let ys = [ctm[3] * y + ctm[5], ctm[3] * (y + h) + ctm[5]];
        if !xs.iter().chain(&ys).all(|x| x.is_finite()) {
            return None;
        }
        let rect = Self {
            left: xs[0].min(xs[1]),
            right: xs[0].max(xs[1]),
            bottom: ys[0].min(ys[1]),
            top: ys[0].max(ys[1]),
        };
        (rect.left < rect.right && rect.bottom < rect.top).then_some(rect)
    }

    fn contains_advance(self, item: &TextItem) -> bool {
        item.is_upright()
            && item.advance_known
            && item.width > 0.0
            && [item.x, item.y, item.width].iter().all(|x| x.is_finite())
            && item.x >= self.left - TOLERANCE
            && item.x + item.width <= self.right + TOLERANCE
            && item.y >= self.bottom - TOLERANCE
            && item.y <= self.top + TOLERANCE
    }

    /// Whether an upright text run of known extent lies wholly outside this
    /// clip, with a quarter of its height to spare on every side: the run's
    /// box starts at the baseline and leaves out descenders, and glyphs may
    /// overhang their advance a little. A run of unknown advance (its extent
    /// is an estimate), a rotated run and a non-text item are never
    /// excluded.
    fn excludes_run(self, item: &TextItem) -> bool {
        if !matches!(item.item_type, ItemType::Text)
            || !item.is_upright()
            || !item.advance_known
            || !(item.width > 0.0 && item.height > 0.0)
            || ![item.x, item.y, item.width, item.height]
                .iter()
                .all(|v| v.is_finite())
        {
            return false;
        }
        let pad = item.height * 0.25;
        item.x + item.width + pad <= self.left
            || item.x - pad >= self.right
            || item.y + item.height + pad <= self.bottom
            || item.y - pad >= self.top
    }
}

#[derive(Clone, Copy, Default)]
enum Clip {
    #[default]
    Unbounded,
    Rect(ClipRect),
    Unknown,
}

#[derive(Default)]
pub(super) struct ClipTracker {
    active: Clip,
    saved: Vec<Clip>,
    // Paths are not part of the saved graphics state. Store at most one rect.
    path: Option<Clip>,
    pending: bool,
    text_clip_pending: bool,
}

impl ClipTracker {
    pub(super) fn rect(&self) -> Option<ClipRect> {
        match self.active {
            Clip::Rect(rect) if !self.text_clip_pending => Some(rect),
            _ => None,
        }
    }

    pub(super) fn observe(&mut self, operator: &str, operands: &[Object], ctm: [f32; 6]) {
        match operator {
            "q" => self.saved.push(self.active),
            "Q" => self.active = self.saved.pop().unwrap_or(Clip::Unknown),
            "cm" if operands.len() != 6
                || !operands
                    .iter()
                    .all(|v| get_number(v).is_some_and(f32::is_finite)) =>
            {
                self.active = Clip::Unknown;
            }
            "re" => {
                self.path = Some(if self.path.is_none() {
                    ClipRect::from_path(operands, ctm).map_or(Clip::Unknown, Clip::Rect)
                } else {
                    Clip::Unknown
                });
            }
            "m" | "l" | "c" | "v" | "y" | "h" => self.path = Some(Clip::Unknown),
            "W" | "W*" => self.pending = true,
            "n" | "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                if self.pending {
                    self.active = match (self.active, self.path) {
                        (Clip::Unbounded, Some(Clip::Rect(rect))) => Clip::Rect(rect),
                        (Clip::Rect(active), Some(Clip::Rect(rect))) => {
                            active.intersection(rect).map_or(Clip::Unknown, Clip::Rect)
                        }
                        _ => Clip::Unknown,
                    };
                }
                self.path = None;
                self.pending = false;
            }
            // Glyph outlines make the effective clip nonrectangular at ET.
            // Abstain immediately as well, including q/Q inside the text object.
            "Tr" if operands
                .first()
                .and_then(get_number)
                .is_some_and(|v| v >= 4.0) =>
            {
                self.active = Clip::Unknown;
                self.text_clip_pending = true;
            }
            "ET" if self.text_clip_pending => {
                self.active = Clip::Unknown;
                self.text_clip_pending = false;
            }
            _ => {}
        }
    }
}

/// Remove from `items` the runs their own clip hides — those
/// `ClipRect::excludes_run` judges wholly outside the rectangle in force
/// when they were shown — keeping `clips` aligned with `items`, and return
/// how many were removed. A run with no established clip (`None`) always
/// stays. Both vectors shrink in step, so call this after any fix-up that
/// indexes into `items`.
/// Whether the clip in force when `item` was shown hides it wholly (see
/// `ClipRect::excludes_run`); a run with no established clip is never
/// hidden. The test `drop_clipped_away_runs` applies, shared with the
/// page's storage-order vote so that a run not on the page decides nothing.
pub(super) fn excluded_by_clip(item: &TextItem, clip: Option<ClipRect>) -> bool {
    clip.is_some_and(|rect| rect.excludes_run(item))
}

pub(super) fn drop_clipped_away_runs(
    items: &mut Vec<TextItem>,
    clips: &mut Vec<Option<ClipRect>>,
) -> usize {
    debug_assert_eq!(items.len(), clips.len());
    let keep: Vec<bool> = items
        .iter()
        .zip(clips.iter())
        .map(|(item, clip)| {
            let excluded = excluded_by_clip(item, *clip);
            if excluded {
                log::trace!(
                    "run painted outside its clip left out: {} chars at ({}, {}) {}x{}",
                    item.text.chars().count(),
                    item.x,
                    item.y,
                    item.width,
                    item.height
                );
            }
            !excluded
        })
        .collect();
    if keep.iter().all(|&kept| kept) {
        return 0;
    }
    let before = items.len();
    let mut kept = keep.iter();
    items.retain(|_| *kept.next().unwrap_or(&true));
    let mut kept = keep.iter();
    clips.retain(|_| *kept.next().unwrap_or(&true));
    before - items.len()
}

pub(super) fn separated_runs(
    previous: &TextItem,
    previous_clip: Option<&ClipRect>,
    next: &TextItem,
    next_clip: Option<&ClipRect>,
) -> bool {
    match (previous_clip, next_clip) {
        (Some(previous_rect), Some(next_rect)) => {
            (previous_rect.right + TOLERANCE < next_rect.left
                || next_rect.right + TOLERANCE < previous_rect.left)
                && previous_rect.contains_advance(previous)
                && next_rect.contains_advance(next)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

    fn apply(tracker: &mut ClipTracker, content: &str, ctm: [f32; 6]) {
        for op in lopdf::content::Content::decode(content.as_bytes())
            .unwrap()
            .operations
        {
            tracker.observe(&op.operator, &op.operands, ctm);
        }
    }

    #[test]
    fn clip_waits_for_path_end_and_intersects_nested_rectangles() {
        let mut tracker = ClipTracker::default();
        apply(&mut tracker, "10 20 80 90 re W", IDENTITY);
        assert!(tracker.rect().is_none());
        apply(&mut tracker, "n q 20 10 100 80 re W* f", IDENTITY);
        let rect = tracker.rect().unwrap();
        assert_eq!(
            (rect.left, rect.bottom, rect.right, rect.top),
            (20.0, 20.0, 90.0, 90.0)
        );
        apply(&mut tracker, "Q", IDENTITY);
        assert_eq!(tracker.rect().unwrap().left, 10.0);
    }

    #[test]
    fn path_coordinates_are_captured_when_constructed() {
        let mut tracker = ClipTracker::default();
        apply(
            &mut tracker,
            "10 20 -5 10 re",
            [-2.0, 0.0, 0.0, 3.0, 50.0, 7.0],
        );
        apply(&mut tracker, "W n", IDENTITY);
        let rect = tracker.rect().unwrap();
        assert_eq!(
            (rect.left, rect.right, rect.bottom, rect.top),
            (30.0, 40.0, 67.0, 97.0)
        );
    }

    #[test]
    fn uncertain_clips_remain_unknown_until_graphics_restore() {
        for path in [
            "0 0 m 10 0 l 10 10 l h W n",
            "0 0 10 10 re 20 0 10 10 re W n",
            "0 0 0 10 re W n",
            "W n",
            "0 0 10 10 re W n 30 30 10 10 re W n",
        ] {
            let mut tracker = ClipTracker::default();
            apply(&mut tracker, "0 0 100 100 re W n q", IDENTITY);
            apply(&mut tracker, path, IDENTITY);
            apply(&mut tracker, "1 1 2 2 re W n", IDENTITY);
            assert!(tracker.rect().is_none(), "{path}");
            apply(&mut tracker, "Q", IDENTITY);
            assert_eq!(tracker.rect().unwrap().right, 100.0);
        }
    }

    #[test]
    fn path_is_not_restored_with_graphics_state() {
        let mut tracker = ClipTracker::default();
        apply(&mut tracker, "q 10 20 30 40 re Q W n", IDENTITY);
        assert_eq!(tracker.rect().unwrap().left, 10.0);
    }

    #[test]
    fn unsupported_transforms_and_text_clips_abstain() {
        for ctm in [
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
            [1.0, 0.1, 0.0, 1.0, 0.0, 0.0],
            [f32::INFINITY, 0.0, 0.0, 1.0, 0.0, 0.0],
        ] {
            let mut tracker = ClipTracker::default();
            apply(&mut tracker, "1 2 30 40 re W n", ctm);
            assert!(tracker.rect().is_none());
        }
        let mut tracker = ClipTracker::default();
        apply(&mut tracker, "0 0 100 100 re W n BT q 4 Tr Q", IDENTITY);
        assert!(tracker.rect().is_none());
        apply(&mut tracker, "ET 1 1 2 2 re W n", IDENTITY);
        assert!(tracker.rect().is_none());
    }

    const CLIP: ClipRect = ClipRect {
        left: 100.0,
        bottom: 200.0,
        right: 300.0,
        top: 400.0,
    };

    fn run(text: &str, x: f32, y: f32, width: f32, height: f32) -> TextItem {
        TextItem {
            text: text.into(),
            x,
            y,
            width,
            height,
            font: "Helvetica".into(),
            font_tag: "F1".into(),
            legacy_symbol_rewrite: false,
            font_size: height,
            page: 1,
            is_bold: false,
            is_italic: false,
            font_weight: None,
            bold_source: None,
            fixed_pitch: None,
            is_underline: false,
            is_strikeout: false,
            rotation: 0.0,
            advance_known: true,
            item_type: ItemType::Text,
            mcid: None,
            baseline_shift: 0.0,
        }
    }

    #[test]
    fn runs_inside_or_straddling_the_clip_are_kept() {
        for (name, item) in [
            ("inside", run("a", 150.0, 250.0, 50.0, 10.0)),
            ("across the left edge", run("a", 80.0, 250.0, 50.0, 10.0)),
            ("across the right edge", run("a", 280.0, 250.0, 50.0, 10.0)),
            ("across the bottom edge", run("a", 150.0, 195.0, 50.0, 10.0)),
            ("across the top edge", run("a", 150.0, 395.0, 50.0, 10.0)),
            ("larger than the clip", run("a", 50.0, 150.0, 400.0, 300.0)),
        ] {
            assert!(!CLIP.excludes_run(&item), "{name}");
        }
    }

    #[test]
    fn runs_wholly_outside_the_clip_are_excluded() {
        for (name, item) in [
            ("left", run("a", 20.0, 250.0, 50.0, 10.0)),
            ("right", run("a", 310.0, 250.0, 50.0, 10.0)),
            ("below", run("a", 150.0, 150.0, 50.0, 10.0)),
            ("above", run("a", 150.0, 410.0, 50.0, 10.0)),
            ("far away", run("a", 1000.0, -500.0, 50.0, 10.0)),
        ] {
            assert!(CLIP.excludes_run(&item), "{name}");
        }
    }

    #[test]
    fn a_quarter_of_the_height_keeps_descenders_and_overhang_visible() {
        // A 10 pt run must clear the clip by 2.5 pt; touching or nearly
        // touching the edge is not enough.
        assert!(!CLIP.excludes_run(&run("a", 150.0, 190.0, 50.0, 10.0)));
        assert!(!CLIP.excludes_run(&run("a", 150.0, 188.0, 50.0, 10.0)));
        assert!(CLIP.excludes_run(&run("a", 150.0, 187.0, 50.0, 10.0)));
        assert!(!CLIP.excludes_run(&run("a", 150.0, 402.0, 50.0, 10.0)));
        assert!(CLIP.excludes_run(&run("a", 150.0, 403.0, 50.0, 10.0)));
        assert!(!CLIP.excludes_run(&run("a", 48.0, 250.0, 50.0, 10.0)));
        assert!(CLIP.excludes_run(&run("a", 47.0, 250.0, 50.0, 10.0)));
        assert!(!CLIP.excludes_run(&run("a", 302.0, 250.0, 50.0, 10.0)));
        assert!(CLIP.excludes_run(&run("a", 303.0, 250.0, 50.0, 10.0)));
    }

    #[test]
    fn uncertain_runs_are_never_excluded() {
        let far = || run("a", 1000.0, 1000.0, 50.0, 10.0);
        let mut estimated = far();
        estimated.advance_known = false;
        let mut rotated = far();
        rotated.rotation = 90.0;
        let mut upside_down = far();
        upside_down.rotation = 180.0;
        let mut image = far();
        image.item_type = ItemType::Image;
        let mut empty = far();
        empty.width = 0.0;
        let mut flat = far();
        flat.height = 0.0;
        let mut nan = far();
        nan.x = f32::NAN;
        for (name, item) in [
            ("estimated advance", estimated),
            ("rotated", rotated),
            ("upside down", upside_down),
            ("image", image),
            ("zero width", empty),
            ("zero height", flat),
            ("non-finite", nan),
        ] {
            assert!(!CLIP.excludes_run(&item), "{name}");
        }
        // Slightly tilted runs still read along +x and are judged by their
        // (exact, advance-known) box.
        let mut tilted = far();
        tilted.rotation = 10.0;
        assert!(CLIP.excludes_run(&tilted));
    }

    #[test]
    fn dropping_keeps_items_and_clips_aligned() {
        let mut items = vec![
            run("kept inside", 150.0, 250.0, 50.0, 10.0),
            run("hidden", 150.0, 150.0, 50.0, 10.0),
            run("no clip, far away", 150.0, 150.0, 50.0, 10.0),
            run("kept inside too", 200.0, 250.0, 50.0, 10.0),
        ];
        let inner = ClipRect {
            left: 190.0,
            bottom: 200.0,
            right: 300.0,
            top: 400.0,
        };
        let mut clips = vec![Some(CLIP), Some(CLIP), None, Some(inner)];
        assert_eq!(drop_clipped_away_runs(&mut items, &mut clips), 1);
        let texts: Vec<&str> = items.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(
            texts,
            ["kept inside", "no clip, far away", "kept inside too"]
        );
        assert_eq!(clips.len(), 3);
        assert_eq!(clips[0].map(|c| c.left), Some(100.0));
        assert!(clips[1].is_none());
        assert_eq!(clips[2].map(|c| c.left), Some(190.0));

        let mut untouched = vec![run("a", 150.0, 250.0, 50.0, 10.0)];
        let mut untouched_clips = vec![Some(CLIP)];
        assert_eq!(
            drop_clipped_away_runs(&mut untouched, &mut untouched_clips),
            0
        );
        assert_eq!(untouched.len(), 1);
    }
}
