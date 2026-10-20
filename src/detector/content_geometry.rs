//! The geometry of the executed-content scan: boxes in user space, the
//! matrices that place them, and where a text object's clip-only text
//! lies for the paint that shows through it.

/// A box in user space, `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct UserBox {
    pub(super) x0: f64,
    pub(super) y0: f64,
    pub(super) x1: f64,
    pub(super) y1: f64,
}

impl UserBox {
    pub(super) fn area(&self) -> f64 {
        (self.x1 - self.x0) * (self.y1 - self.y0)
    }

    /// The smallest box holding both.
    pub(super) fn union(&self, other: &UserBox) -> UserBox {
        UserBox {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// `None` when the boxes do not overlap.
    pub(super) fn intersect(&self, other: &UserBox) -> Option<UserBox> {
        let clipped = UserBox {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (clipped.x1 > clipped.x0 && clipped.y1 > clipped.y0).then_some(clipped)
    }

    /// Whether the boxes touch — their edges included, so a line along
    /// an edge touches.
    pub(super) fn touches(&self, other: &UserBox) -> bool {
        self.x0 <= other.x1 && self.x1 >= other.x0 && self.y0 <= other.y1 && self.y1 >= other.y0
    }

    /// The part of this box within `other`, which may have no area — a
    /// line, or a point; `None` when they do not touch.
    pub(super) fn clamped(&self, other: &UserBox) -> Option<UserBox> {
        self.touches(other).then(|| UserBox {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        })
    }
}

/// `first` applied before `second`: the product `cm` forms when it puts
/// a matrix before the one in force, and the text matrix forms under it.
pub(super) fn multiply(
    [a1, b1, c1, d1, e1, f1]: [f64; 6],
    [a2, b2, c2, d2, e2, f2]: [f64; 6],
) -> [f64; 6] {
    [
        a1 * a2 + b1 * c2,
        a1 * b2 + b1 * d2,
        c1 * a2 + d1 * c2,
        c1 * b2 + d1 * d2,
        e1 * a2 + f1 * c2 + e2,
        e1 * b2 + f1 * d2 + f2,
    ]
}

/// The bounding box of `[x0 y0 x1 y1]` under `matrix`; `None` when it is
/// not finite.
pub(super) fn box_under(
    [a, b, c, d, e, f]: [f64; 6],
    [x0, y0, x1, y1]: [f64; 4],
) -> Option<UserBox> {
    let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
    let xs = corners.map(|(x, y)| a * x + c * y + e);
    let ys = corners.map(|(x, y)| b * x + d * y + f);
    if !xs.iter().chain(&ys).all(|v| v.is_finite()) {
        return None;
    }
    Some(UserBox {
        x0: xs.iter().copied().fold(f64::INFINITY, f64::min),
        y0: ys.iter().copied().fold(f64::INFINITY, f64::min),
        x1: xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        y1: ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    })
}

/// Where the glyphs of a text object's clip-only text lie, for the paint
/// that shows through them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Reach {
    /// Wherever paint lands: a show whose position or font size the scan
    /// did not see.
    Anywhere,
    /// Outside the clip in force, every glyph of it: no paint shows.
    Nowhere,
    /// Within this box, inside the clip in force.
    Within(UserBox),
}

impl Reach {
    /// The reach of both together.
    pub(super) fn join(self, other: Reach) -> Reach {
        match (self, other) {
            (Reach::Anywhere, _) | (_, Reach::Anywhere) => Reach::Anywhere,
            (Reach::Nowhere, reach) | (reach, Reach::Nowhere) => reach,
            (Reach::Within(a), Reach::Within(b)) => Reach::Within(a.union(&b)),
        }
    }

    /// Whether a paint landing on `landed` — `None` for one of unknown
    /// extent — shows through the glyphs.
    pub(super) fn shown_by(self, landed: Option<UserBox>) -> bool {
        match (self, landed) {
            (Reach::Nowhere, _) => false,
            (Reach::Anywhere, _) | (_, None) => true,
            (Reach::Within(reach), Some(landed)) => reach.touches(&landed),
        }
    }
}

/// The clip-only text of one text object whose clip is in force: how
/// many text-showing operators built it, and where its glyphs lie.
#[derive(Clone, Copy, Debug)]
pub(super) struct ClipText {
    pub(super) ops: u32,
    pub(super) reach: Reach,
}

/// Where the open text object shows next: the text matrix, and the line
/// matrix the next line starts from.
#[derive(Clone, Copy)]
pub(super) struct TextPosition {
    pub(super) matrix: [f64; 6],
    pub(super) line: [f64; 6],
}
