//! Device-space geometry of shown text runs.
//!
//! Shared by the page content-stream parser and the Form XObject parser so
//! both stamp identical boxes and baseline angles on their `TextItem`s.

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
/// The up vector is the advance direction turned 90° counter-clockwise, not
/// the matrix's own y axis: glyphs stand on the left of their baseline in
/// every real layout, whereas the y axis flips sign in legacy dvips output
/// (a `[s 0 0 -s]` text matrix undone by a mirrored Type3 FontMatrix) and
/// leans in synthetic-italic shears — following it would drop those boxes
/// one em below the baseline or widen them by the slant, neither of which
/// the baseline-anchored box ever did. An unknown advance contributes no
/// extent, so a horizontal run keeps `width == 0` as the "unknown" signal
/// `effective_width` relies on, while a vertical run still reports its em
/// width.
pub(crate) fn run_geometry(combined: &[f32; 6], advance_ts: Option<f32>, em: f32) -> RunGeometry {
    let (x0, y0) = (combined[4], combined[5]);
    let advance = advance_ts.unwrap_or(0.0);
    let (ax, ay) = (advance * combined[0], advance * combined[1]);
    // One em perpendicular to the baseline, on its left: (a, b) turned 90°
    // counter-clockwise and scaled to `em` (which already carries the
    // matrix's scale, so anisotropic scaling keeps `height == em`).
    let axis_len = combined[0].hypot(combined[1]);
    let (ux, uy) = if axis_len > f32::EPSILON {
        (-combined[1] / axis_len * em, combined[0] / axis_len * em)
    } else {
        (0.0, em)
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
    fn run_geometry_without_advance_keeps_em_width_for_vertical_runs() {
        // No font widths: an upright run keeps `width == 0` as the
        // "advance unknown" signal, a vertical run still owns its em column.
        let vertical = run_geometry(&[0.0, 1.0, -1.0, 0.0, 100.0, 100.0], None, 10.0);
        assert_eq!(
            (vertical.x, vertical.y, vertical.width, vertical.height),
            (90.0, 100.0, 10.0, 0.0)
        );
        let upright = run_geometry(&[1.0, 0.0, 0.0, 1.0, 100.0, 100.0], None, 10.0);
        assert_eq!(
            (upright.x, upright.y, upright.width, upright.height),
            (100.0, 100.0, 0.0, 10.0)
        );
    }

    #[test]
    fn run_geometry_anisotropic_scale_keeps_rendered_em_height() {
        // [2 0 0 1]: horizontally stretched text. The em passed in is the
        // rendered size, and the box height must stay exactly that.
        let stretched = run_geometry(&[2.0, 0.0, 0.0, 1.0, 0.0, 0.0], Some(10.0), 24.0);
        assert_eq!(
            (stretched.width, stretched.height, stretched.rotation),
            (20.0, 24.0, 0.0)
        );
    }

    #[test]
    fn run_geometry_keeps_the_box_above_a_y_flipped_baseline() {
        // dvips output: `[s 0 0 -s]` text matrix, glyphs flipped back upright
        // by the Type3 FontMatrix. The baseline-anchored box must not drop
        // below the baseline just because the matrix's y axis points down —
        // that would put Type3 text one em off the Type1 math on its line.
        let flipped = run_geometry(&[1.0, 0.0, 0.0, -1.0, 100.0, 500.0], Some(30.0), 10.0);
        assert_eq!(
            (
                flipped.x,
                flipped.y,
                flipped.width,
                flipped.height,
                flipped.rotation
            ),
            (100.0, 500.0, 30.0, 10.0, 0.0)
        );
        // Synthetic italics shear the y axis; the box stays em-high and
        // advance-wide instead of growing by the slant.
        let sheared = run_geometry(&[1.0, 0.0, 0.34, 1.0, 100.0, 500.0], Some(30.0), 10.0);
        assert_eq!(
            (sheared.x, sheared.y, sheared.width, sheared.height),
            (100.0, 500.0, 30.0, 10.0)
        );
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
