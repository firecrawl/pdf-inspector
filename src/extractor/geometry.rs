//! Device-space geometry of shown text runs.
//!
//! Shared by the page content-stream parser and the Form XObject parser so
//! both stamp identical boxes and baseline angles on their `TextItem`s.

/// Which way a page's coordinate frame was turned so that predominantly
/// rotated text reads along +x (see `content_stream::correct_rotated_page`).
/// Region boxes given in page coordinates must be turned the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageRotation {
    /// Text reads along +x; coordinates are plain page coordinates.
    Upright,
    /// Most runs read bottom-to-top (90°, `Tm = [0 b -b 0]`): the frame was
    /// turned so they read along +x, mapping `(x, y) → (y, -x)`.
    Ccw,
    /// Most runs read top-to-bottom (270°, `Tm = [0 -b b 0]`): the frame was
    /// turned the other way, mapping `(x, y) → (-y, x)`.
    Cw,
}

impl PageRotation {
    /// Degrees added to a run's baseline angle when its page frame is
    /// turned: the dominant runs land on `0`.
    pub(crate) fn baseline_rebase_degrees(self) -> f32 {
        match self {
            PageRotation::Upright => 0.0,
            PageRotation::Ccw => -90.0,
            PageRotation::Cw => 90.0,
        }
    }

    /// Turn a point with the page frame.
    pub(crate) fn rotate_point(self, x: f32, y: f32) -> (f32, f32) {
        match self {
            PageRotation::Upright => (x, y),
            PageRotation::Ccw => (y, -x),
            PageRotation::Cw => (-y, x),
        }
    }

    /// Turn an axis-aligned box with the page frame: the negated axis's far
    /// edge becomes the new near edge and the extents swap. Extents may be
    /// negative (rects drawn under a reflected CTM), so both edges are
    /// normalised first and the result always has non-negative extents.
    pub(crate) fn rotate_box(self, x: &mut f32, y: &mut f32, width: &mut f32, height: &mut f32) {
        let (x0, x1) = (x.min(*x + *width), x.max(*x + *width));
        let (y0, y1) = (y.min(*y + *height), y.max(*y + *height));
        let (new_x, new_y) = match self {
            PageRotation::Upright => return,
            PageRotation::Ccw => (y0, -x1),
            PageRotation::Cw => (-y1, x0),
        };
        *x = new_x;
        *y = new_y;
        *width = y1 - y0;
        *height = x1 - x0;
    }
}

/// Text rise (Ts) displaces the glyph origin by (0, rise) in unscaled text
/// space — per the rendering-matrix definition it sits left of Tm, so the
/// offset maps through the text matrix's y column. Rise never contributes
/// to the advance, so callers apply it only to the rendering position and
/// keep advancing the unshifted text matrix.
pub(crate) fn rise_adjusted(tm: &[f32; 6], rise: f32) -> [f32; 6] {
    if rise == 0.0 {
        return *tm;
    }
    [
        tm[0],
        tm[1],
        tm[2],
        tm[3],
        tm[4] + rise * tm[2],
        tm[5] + rise * tm[3],
    ]
}

/// Axis-aligned device-space box of one shown run plus its baseline angle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RunGeometry {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) rotation: f32,
}

/// Compute the axis-aligned box a shown run occupies in device space.
///
/// `combined` is the (rise-adjusted) text matrix × CTM at the run's start,
/// `advance_ts` the run's advance in text-space units when the font carries
/// width information, and `em` the rendered em size. The glyphs occupy the
/// rectangle spanned by the device-space advance vector and a one-em glyph
/// up vector; its bounding box is `[x, x+advance] × [y, y+em]` for upright
/// text — exactly what callers always produced — but stays tall and thin
/// for a rotated run. Projecting the advance onto x alone left a 90° run
/// with `width == 0`, which downstream code reads as "advance unknown" and
/// replaces with a character-count estimate: a vertical margin stamp became
/// a page-wide phantom horizontal line.
///
/// The up vector is one em perpendicular to the baseline, on the side the
/// matrix's own y axis points to. Perpendicular rather than the y axis
/// itself, because synthetic-italic shears lean that axis and would widen
/// the box by the slant and shrink its height, which the baseline-anchored
/// box never did. The y axis still decides the side: a producer that
/// mirrors x for right-to-left text (`[-s 0 0 s]`) keeps its glyphs above
/// the baseline, and a genuinely y-flipped matrix renders them below.
/// `glyph_up_flipped` undoes that choice for Type3 fonts whose FontMatrix
/// mirrors y: dvips/PK bitmap fonts declare `[1 0 0 -1 0 0]` and pair it
/// with a y-flipped text matrix so the glyphs come out upright, so their
/// box belongs above the baseline although the matrix says otherwise.
///
/// An unknown advance contributes no extent, so a horizontal run keeps
/// `width == 0` as the "unknown" signal `effective_width` relies on, while a
/// vertical run still reports its em width (and `height == 0`, which
/// `effective_height` estimates).
pub(crate) fn run_geometry(
    combined: &[f32; 6],
    advance_ts: Option<f32>,
    em: f32,
    glyph_up_flipped: bool,
) -> RunGeometry {
    let (x0, y0) = (combined[4], combined[5]);
    let advance = advance_ts.unwrap_or(0.0);
    let (ax, ay) = (advance * combined[0], advance * combined[1]);
    let axis_len = combined[0].hypot(combined[1]);
    let (ux, uy) = if axis_len > f32::EPSILON {
        // Unit perpendicular to the baseline (the advance turned 90° CCW),
        // then the y axis's (c, d) component along it: its sign is the side
        // the glyphs stand on, its size the em's true perpendicular extent.
        // `em` carries the matrix's larger scale (that is what `font_size`
        // reports), so a stretched `[2 0 0 1]` matrix must be scaled back
        // down or the box would be twice as tall as the glyphs.
        let (px, py) = (-combined[1] / axis_len, combined[0] / axis_len);
        let y_axis_perp = combined[2] * px + combined[3] * py;
        let max_scale = axis_len.max(combined[2].hypot(combined[3]));
        let em_perp = if y_axis_perp.abs() > f32::EPSILON && max_scale > f32::EPSILON {
            em * y_axis_perp.abs() / max_scale
        } else {
            em
        };
        let y_axis_side = if y_axis_perp < 0.0 { -1.0 } else { 1.0 };
        let side = if glyph_up_flipped {
            -y_axis_side
        } else {
            y_axis_side
        };
        (px * em_perp * side, py * em_perp * side)
    } else {
        (0.0, if glyph_up_flipped { -em } else { em })
    };
    let xs = [x0, x0 + ax, x0 + ux, x0 + ax + ux];
    let ys = [y0, y0 + ay, y0 + uy, y0 + ay + uy];
    let x_min = xs.iter().copied().fold(f32::INFINITY, f32::min);
    let x_max = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let y_min = ys.iter().copied().fold(f32::INFINITY, f32::min);
    let y_max = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    RunGeometry {
        x: x_min,
        y: y_min,
        width: x_max - x_min,
        height: y_max - y_min,
        rotation: baseline_rotation(combined[0], combined[1]),
    }
}

/// Angle of the text-space x axis `(a, b)` in device space, in degrees
/// counter-clockwise from +x and normalised to `[0, 360)`. Float noise
/// within a hundredth of a degree of a whole degree is snapped so
/// rotation-only matrices report exact `0`, `90`, `180`, `270`.
pub(crate) fn baseline_rotation(a: f32, b: f32) -> f32 {
    if a == 0.0 && b == 0.0 {
        return 0.0;
    }
    let degrees = normalize_degrees(b.atan2(a).to_degrees());
    let whole = degrees.round();
    if (degrees - whole).abs() < 0.01 {
        normalize_degrees(whole)
    } else {
        degrees
    }
}

/// Wrap an angle into `[0, 360)`, mapping `-0.0` and `360.0` to `0.0`.
pub(crate) fn normalize_degrees(degrees: f32) -> f32 {
    let wrapped = degrees.rem_euclid(360.0);
    if wrapped >= 360.0 || wrapped == 0.0 {
        0.0
    } else {
        wrapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_rotation_turns_boxes_and_points_consistently() {
        // A 90° run (box 12 wide, 36 tall, baseline at x = 200) turned CCW:
        // x = run start, y = -baseline, extents swapped.
        let (mut x, mut y, mut w, mut h) = (188.0, 100.0, 12.0, 36.0);
        PageRotation::Ccw.rotate_box(&mut x, &mut y, &mut w, &mut h);
        assert_eq!((x, y, w, h), (100.0, -200.0, 36.0, 12.0));
        assert_eq!(
            PageRotation::Ccw.rotate_point(200.0, 100.0),
            (100.0, -200.0)
        );
        // A 270° run (baseline at x = 580, running down from y = 700) turned
        // CW: x = -(run end), y = baseline x.
        let (mut x, mut y, mut w, mut h) = (580.0, 664.0, 10.0, 36.0);
        PageRotation::Cw.rotate_box(&mut x, &mut y, &mut w, &mut h);
        assert_eq!((x, y, w, h), (-700.0, 580.0, 36.0, 10.0));
        assert_eq!(PageRotation::Cw.rotate_point(580.0, 700.0), (-700.0, 580.0));
        let (mut x, mut y, mut w, mut h) = (1.0, 2.0, 3.0, 4.0);
        PageRotation::Upright.rotate_box(&mut x, &mut y, &mut w, &mut h);
        assert_eq!((x, y, w, h), (1.0, 2.0, 3.0, 4.0));
        assert_eq!(PageRotation::Ccw.baseline_rebase_degrees(), -90.0);
        assert_eq!(PageRotation::Cw.baseline_rebase_degrees(), 90.0);
    }

    #[test]
    fn page_rotation_normalises_negative_extents() {
        // A rect drawn under a reflected CTM: anchor (100, 200), extents
        // (-20, -10) span x ∈ [80, 100], y ∈ [190, 200]. Both turns must
        // produce the same box as for the normalised rect.
        for rotation in [PageRotation::Ccw, PageRotation::Cw] {
            let (mut x, mut y, mut w, mut h) = (100.0, 200.0, -20.0, -10.0);
            rotation.rotate_box(&mut x, &mut y, &mut w, &mut h);
            let (mut nx, mut ny, mut nw, mut nh) = (80.0, 190.0, 20.0, 10.0);
            rotation.rotate_box(&mut nx, &mut ny, &mut nw, &mut nh);
            assert_eq!((x, y, w, h), (nx, ny, nw, nh), "{rotation:?}");
            assert!(w > 0.0 && h > 0.0);
        }
        let (mut x, mut y, mut w, mut h) = (100.0, 200.0, -20.0, -10.0);
        PageRotation::Ccw.rotate_box(&mut x, &mut y, &mut w, &mut h);
        assert_eq!((x, y, w, h), (190.0, -100.0, 10.0, 20.0));
    }

    #[test]
    fn run_geometry_without_advance_keeps_em_width_for_vertical_runs() {
        // No font widths: an upright run keeps `width == 0` as the
        // "advance unknown" signal, a vertical run still owns its em column.
        let vertical = run_geometry(&[0.0, 1.0, -1.0, 0.0, 100.0, 100.0], None, 10.0, false);
        assert_eq!(
            (vertical.x, vertical.y, vertical.width, vertical.height),
            (90.0, 100.0, 10.0, 0.0)
        );
        let upright = run_geometry(&[1.0, 0.0, 0.0, 1.0, 100.0, 100.0], None, 10.0, false);
        assert_eq!(
            (upright.x, upright.y, upright.width, upright.height),
            (100.0, 100.0, 0.0, 10.0)
        );
    }

    #[test]
    fn run_geometry_takes_the_em_height_from_the_y_axis() {
        // [2 0 0 1]: horizontally stretched 12pt text. `font_size` reports
        // the larger scale (24), but the glyphs are only 12pt tall.
        let stretched = run_geometry(&[2.0, 0.0, 0.0, 1.0, 0.0, 0.0], Some(10.0), 24.0, false);
        assert_eq!(
            (stretched.width, stretched.height, stretched.rotation),
            (20.0, 12.0, 0.0)
        );
        // [1 0 0 2]: vertically stretched — the y axis carries the scale.
        let tall = run_geometry(&[1.0, 0.0, 0.0, 2.0, 0.0, 0.0], Some(10.0), 24.0, false);
        assert_eq!((tall.width, tall.height), (10.0, 24.0));
        // Uniform scale and rotation are unaffected.
        let turned = run_geometry(&[0.0, 2.0, -2.0, 0.0, 0.0, 0.0], Some(10.0), 24.0, false);
        assert_eq!((turned.width, turned.height), (24.0, 20.0));
    }

    #[test]
    fn run_geometry_picks_the_baseline_side_from_the_matrix_and_font() {
        // dvips output: `[s 0 0 -s]` text matrix, glyphs flipped back upright
        // by the `[1 0 0 -1]` Type3 FontMatrix. The box must stay above the
        // baseline — putting it one em below would separate Type3 text from
        // the Type1 math on its line.
        let dvips = run_geometry(&[1.0, 0.0, 0.0, -1.0, 100.0, 500.0], Some(30.0), 10.0, true);
        assert_eq!(
            (dvips.x, dvips.y, dvips.width, dvips.height, dvips.rotation),
            (100.0, 500.0, 30.0, 10.0, 0.0)
        );
        // The same matrix with an ordinary font really does render the
        // glyphs upside down, hanging below the baseline.
        let hanging = run_geometry(
            &[1.0, 0.0, 0.0, -1.0, 100.0, 500.0],
            Some(30.0),
            10.0,
            false,
        );
        assert_eq!((hanging.y, hanging.height), (490.0, 10.0));
        // A producer mirroring x for right-to-left text keeps its glyphs
        // above the baseline: the box runs left from the start point and up.
        let mirrored = run_geometry(
            &[-1.0, 0.0, 0.0, 1.0, 300.0, 500.0],
            Some(30.0),
            10.0,
            false,
        );
        assert_eq!(
            (
                mirrored.x,
                mirrored.y,
                mirrored.width,
                mirrored.height,
                mirrored.rotation
            ),
            (270.0, 500.0, 30.0, 10.0, 180.0)
        );
        // Synthetic italics shear the y axis; the box stays em-high and
        // advance-wide instead of growing by the slant. `em` is the rendered
        // size, which for this matrix carries the y axis's 1.056 length.
        let em = 10.0 * 0.34_f32.hypot(1.0);
        let sheared = run_geometry(&[1.0, 0.0, 0.34, 1.0, 100.0, 500.0], Some(30.0), em, false);
        assert_eq!((sheared.x, sheared.y, sheared.width), (100.0, 500.0, 30.0));
        assert!(
            (sheared.height - 10.0).abs() < 1e-3,
            "height = {}",
            sheared.height
        );
        // Rotations keep the glyph side with the matrix: 90° runs stand to
        // the left of their baseline, 270° runs to the right.
        let ccw = run_geometry(
            &[0.0, 1.0, -1.0, 0.0, 100.0, 100.0],
            Some(30.0),
            10.0,
            false,
        );
        assert_eq!((ccw.x, ccw.width), (90.0, 10.0));
        let cw = run_geometry(
            &[0.0, -1.0, 1.0, 0.0, 100.0, 100.0],
            Some(30.0),
            10.0,
            false,
        );
        assert_eq!((cw.x, cw.width), (100.0, 10.0));
    }

    #[test]
    fn baseline_rotation_reports_exact_cardinals_and_fractional_skews() {
        assert_eq!(baseline_rotation(1.0, 0.0), 0.0);
        assert_eq!(baseline_rotation(12.0, 0.0), 0.0);
        assert_eq!(baseline_rotation(0.0, 1.0), 90.0);
        assert_eq!(baseline_rotation(-1.0, 0.0), 180.0);
        assert_eq!(baseline_rotation(0.0, -1.0), 270.0);
        assert_eq!(baseline_rotation(0.0, 0.0), 0.0);
        assert!((baseline_rotation(1.0, 1.0) - 45.0).abs() < 1e-3);
        let skew = baseline_rotation(1.0, 0.01);
        assert!(skew > 0.5 && skew < 0.6, "skew = {skew}");
        // Float noise snaps to the cardinal instead of reporting 359.99999.
        assert_eq!(baseline_rotation(1.0, -1e-7), 0.0);
        assert_eq!(normalize_degrees(-90.0), 270.0);
        assert_eq!(normalize_degrees(360.0), 0.0);
    }
}
