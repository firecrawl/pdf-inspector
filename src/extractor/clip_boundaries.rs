//! Conservative clipping evidence for preserving independent text runs.
//!
//! This does not clip extracted text or infer cells. Only a single finite,
//! axis-aligned rectangle can establish a boundary; unknown paths retain the
//! existing merge behavior, even if a later rectangle narrows their clip.

use super::get_number;
use crate::types::TextItem;
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
}
