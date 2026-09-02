//! Device-space geometry of shown text runs.
//!
//! Shared by the page content-stream parser and the Form XObject parser so
//! both stamp identical boxes and baseline angles on their `TextItem`s.

/// Which way a page's coordinate frame was turned so that predominantly
/// rotated text reads along +x.
///
/// A page whose text mostly runs vertically is re-based so its dominant runs
/// read left-to-right: the items' `x`/`y`/`width`/`height` and `rotation`
/// are then expressed in the turned frame, not in page coordinates. Region
/// boxes given in page coordinates must be turned the same way —
/// `collect_text_in_region_in_frame` does that, and
/// `extract_text_with_positions_and_rotations_mem` reports each page's turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageRotation {
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
    /// normalised first and the result always has non-negative extents —
    /// for an upright page too, where the box otherwise stays put.
    pub(crate) fn rotate_box(self, x: &mut f32, y: &mut f32, width: &mut f32, height: &mut f32) {
        let (x0, x1) = (x.min(*x + *width), x.max(*x + *width));
        let (y0, y1) = (y.min(*y + *height), y.max(*y + *height));
        let (new_x, new_y, new_width, new_height) = match self {
            PageRotation::Upright => (x0, y0, x1 - x0, y1 - y0),
            PageRotation::Ccw => (y0, -x1, y1 - y0, x1 - x0),
            PageRotation::Cw => (-y1, x0, y1 - y0, x1 - x0),
        };
        *x = new_x;
        *y = new_y;
        *width = new_width;
        *height = new_height;
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
    /// Whether `advance_ts` was known: see `TextItem::advance_known`.
    pub(crate) advance_known: bool,
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
/// An unknown advance (a font without width metrics) is replaced by
/// `fallback_advance_ts`, the caller's half-an-em-per-glyph estimate in
/// text-space units, so the box still lies where the text plausibly is —
/// laid along the baseline whichever way it runs, which a consumer adding an
/// estimate on the +x side could not get right for runs reading towards -x —
/// and `advance_known` records that the extent is an estimate.
/// The device-space direction a run reads in: the text matrix's x axis,
/// turned around when the `Tf` size is negative (a negative size negates the
/// glyph matrix). Page-rotation votes take this, as `run_geometry` does, so a
/// vertical run drawn at a negative size never votes against its own items.
pub(crate) fn reading_direction(combined: &[f32; 6], font_size: f32) -> (f32, f32) {
    if font_size < 0.0 {
        (-combined[0], -combined[1])
    } else {
        (combined[0], combined[1])
    }
}

/// `em` is the rendered font size, negative when the `Tf` size was.
pub(crate) fn run_geometry(
    combined: &[f32; 6],
    advance_ts: Option<f32>,
    fallback_advance_ts: f32,
    em: f32,
    glyph_up_flipped: bool,
) -> RunGeometry {
    let (x0, y0) = (combined[4], combined[5]);
    let (advance, advance_known) = match advance_ts {
        Some(advance) => (advance, true),
        None => (fallback_advance_ts, false),
    };
    // A negative `em` is the caller saying the `Tf` size was negative: that
    // turns the glyph matrix around, so the run reads backwards and its
    // glyphs stand on the other side. A negative *advance* alone (tight
    // character spacing, TJ positioning) only moves the cursor backwards
    // over upright glyphs and changes neither.
    let reversed = em < 0.0;
    let em = em.abs();
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
        let side = if glyph_up_flipped != reversed {
            -y_axis_side
        } else {
            y_axis_side
        };
        (px * em_perp * side, py * em_perp * side)
    } else {
        (
            0.0,
            if glyph_up_flipped != reversed {
                -em
            } else {
                em
            },
        )
    };
    // The reading direction, or for a reflected matrix (negative
    // determinant) the orientation of the glyphs: a reflection has no
    // rotation — its reading direction and its glyphs differ by a half turn
    // — so the run reports how its glyphs stand. The mirrored-x matrix some
    // producers paint right-to-left text with therefore reads as an upright
    // run (0), a y-flipped matrix as an upside-down one (180); the box covers
    // the run either way. The glyphs' "right" is their up vector turned 90°
    // clockwise.
    let (dir_x, dir_y) = if reversed {
        (-combined[0], -combined[1])
    } else {
        (combined[0], combined[1])
    };
    let det = combined[0] * combined[3] - combined[1] * combined[2];
    let rotation = if det < 0.0 {
        let up_sign = if glyph_up_flipped != reversed {
            -1.0
        } else {
            1.0
        };
        let (up_x, up_y) = (combined[2] * up_sign, combined[3] * up_sign);
        baseline_rotation(up_y, -up_x)
    } else {
        baseline_rotation(dir_x, dir_y)
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
        rotation,
        advance_known,
    }
}

/// Advance estimate from the decoded text alone, in text-space units: half
/// an em per character. The parsers estimate from the painted codes instead
/// (`content_stream::estimated_string_advance_ts`, where a ligature is one
/// glyph and spacing counts) and fall back to this when no codes were seen.
/// `font_size_ts` is the em in text space (the `Tf` size, times the Type3
/// scale where one applies).
pub(crate) fn estimated_advance_ts(text: &str, font_size_ts: f32) -> f32 {
    estimated_advance_for_glyphs(text.chars().count(), font_size_ts)
}

/// The same estimate from a glyph count — for ActualText spans, whose
/// replacement string can be longer or shorter than what was painted.
pub(crate) fn estimated_advance_for_glyphs(glyphs: usize, font_size_ts: f32) -> f32 {
    glyphs as f32 * 0.5 * font_size_ts
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
        // An upright page keeps the box in place but still normalises it.
        let (mut x, mut y, mut w, mut h) = (100.0, 200.0, -20.0, -10.0);
        PageRotation::Upright.rotate_box(&mut x, &mut y, &mut w, &mut h);
        assert_eq!((x, y, w, h), (80.0, 190.0, 20.0, 10.0));
    }

    #[test]
    fn run_geometry_without_advance_keeps_em_width_for_vertical_runs() {
        // No font widths: an upright run keeps `width == 0` as the
        // "advance unknown" signal, a vertical run still owns its em column.
        let vertical = run_geometry(&[0.0, 1.0, -1.0, 0.0, 100.0, 100.0], None, 0.0, 10.0, false);
        assert_eq!(
            (vertical.x, vertical.y, vertical.width, vertical.height),
            (90.0, 100.0, 10.0, 0.0)
        );
        assert!(!vertical.advance_known);
        let upright = run_geometry(&[1.0, 0.0, 0.0, 1.0, 100.0, 100.0], None, 0.0, 10.0, false);
        assert_eq!(
            (upright.x, upright.y, upright.width, upright.height),
            (100.0, 100.0, 0.0, 10.0)
        );
        assert!(!upright.advance_known);
        // A font that reports a zero advance is not "unknown".
        let zero = run_geometry(
            &[1.0, 0.0, 0.0, 1.0, 100.0, 100.0],
            Some(0.0),
            0.0,
            10.0,
            false,
        );
        assert!(zero.advance_known);
        assert_eq!((zero.width, zero.height), (0.0, 10.0));
    }

    #[test]
    fn run_geometry_lays_the_fallback_advance_along_the_run() {
        // Four glyphs without metrics at a 10pt em: a 20pt estimate, laid in
        // the direction the run reads so the box sits on the text — left of
        // the origin for 180°, below it for 270°.
        let upright = run_geometry(&[1.0, 0.0, 0.0, 1.0, 100.0, 100.0], None, 20.0, 10.0, false);
        assert!(!upright.advance_known);
        assert_eq!(
            (upright.x, upright.y, upright.width, upright.height),
            (100.0, 100.0, 20.0, 10.0)
        );
        let flipped = run_geometry(
            &[-1.0, 0.0, 0.0, 1.0, 100.0, 100.0],
            None,
            20.0,
            10.0,
            false,
        );
        assert_eq!((flipped.x, flipped.width), (80.0, 20.0));
        assert!(!flipped.advance_known);
        let down = run_geometry(
            &[0.0, -1.0, 1.0, 0.0, 100.0, 100.0],
            None,
            20.0,
            10.0,
            false,
        );
        assert_eq!(
            (down.y, down.height, down.x, down.width),
            (80.0, 20.0, 100.0, 10.0)
        );
        let up = run_geometry(
            &[0.0, 1.0, -1.0, 0.0, 100.0, 100.0],
            None,
            20.0,
            10.0,
            false,
        );
        assert_eq!((up.y, up.height, up.x, up.width), (100.0, 20.0, 90.0, 10.0));
        assert_eq!(estimated_advance_ts("abcd", 12.0), 24.0);
    }

    #[test]
    fn run_geometry_takes_the_em_height_from_the_y_axis() {
        // [2 0 0 1]: horizontally stretched 12pt text. `font_size` reports
        // the larger scale (24), but the glyphs are only 12pt tall.
        let stretched = run_geometry(
            &[2.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            Some(10.0),
            0.0,
            24.0,
            false,
        );
        assert_eq!(
            (stretched.width, stretched.height, stretched.rotation),
            (20.0, 12.0, 0.0)
        );
        // [1 0 0 2]: vertically stretched — the y axis carries the scale.
        let tall = run_geometry(
            &[1.0, 0.0, 0.0, 2.0, 0.0, 0.0],
            Some(10.0),
            0.0,
            24.0,
            false,
        );
        assert_eq!((tall.width, tall.height), (10.0, 24.0));
        // Uniform scale and rotation are unaffected.
        let turned = run_geometry(
            &[0.0, 2.0, -2.0, 0.0, 0.0, 0.0],
            Some(10.0),
            0.0,
            24.0,
            false,
        );
        assert_eq!((turned.width, turned.height), (24.0, 20.0));
    }

    #[test]
    fn run_geometry_picks_the_baseline_side_from_the_matrix_and_font() {
        // dvips output: `[s 0 0 -s]` text matrix, glyphs flipped back upright
        // by the `[1 0 0 -1]` Type3 FontMatrix. The box must stay above the
        // baseline — putting it one em below would separate Type3 text from
        // the Type1 math on its line.
        let dvips = run_geometry(
            &[1.0, 0.0, 0.0, -1.0, 100.0, 500.0],
            Some(30.0),
            0.0,
            10.0,
            true,
        );
        assert_eq!(
            (dvips.x, dvips.y, dvips.width, dvips.height, dvips.rotation),
            (100.0, 500.0, 30.0, 10.0, 0.0)
        );
        // The same matrix with an ordinary font really does render the
        // glyphs upside down, hanging below the baseline.
        let hanging = run_geometry(
            &[1.0, 0.0, 0.0, -1.0, 100.0, 500.0],
            Some(30.0),
            0.0,
            10.0,
            false,
        );
        assert_eq!((hanging.y, hanging.height), (490.0, 10.0));
        // A producer mirroring x for right-to-left text keeps its glyphs
        // above the baseline: the box runs left from the start point and up,
        // and the run reports its upright glyphs, not a half turn.
        let mirrored = run_geometry(
            &[-1.0, 0.0, 0.0, 1.0, 300.0, 500.0],
            Some(30.0),
            0.0,
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
            (270.0, 500.0, 30.0, 10.0, 0.0)
        );
        // Synthetic italics shear the y axis; the box stays em-high and
        // advance-wide instead of growing by the slant. `em` is the rendered
        // size, which for this matrix carries the y axis's 1.056 length.
        let em = 10.0 * 0.34_f32.hypot(1.0);
        let sheared = run_geometry(
            &[1.0, 0.0, 0.34, 1.0, 100.0, 500.0],
            Some(30.0),
            0.0,
            em,
            false,
        );
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
            0.0,
            10.0,
            false,
        );
        assert_eq!((ccw.x, ccw.width), (90.0, 10.0));
        let cw = run_geometry(
            &[0.0, -1.0, 1.0, 0.0, 100.0, 100.0],
            Some(30.0),
            0.0,
            10.0,
            false,
        );
        assert_eq!((cw.x, cw.width), (100.0, 10.0));
    }

    #[test]
    fn run_geometry_reports_glyph_orientation_for_reflected_matrices() {
        // `[-1 0 0 1]` mirrors x: the advance runs left, the glyphs stand
        // upright — no rotation maps one onto the other, so the run reports
        // how its glyphs stand and its box still covers the advance.
        let mirrored = run_geometry(
            &[-1.0, 0.0, 0.0, 1.0, 300.0, 500.0],
            Some(21.6),
            0.0,
            12.0,
            false,
        );
        assert_eq!(mirrored.rotation, 0.0);
        assert!((mirrored.x - 278.4).abs() < 1e-3 && mirrored.y == 500.0);
        assert!((mirrored.width - 21.6).abs() < 1e-3 && mirrored.height == 12.0);

        // dvips Type3 output: a y-flipped matrix whose font matrix flips the
        // glyphs back (`glyph_up_flipped`) — upright as well.
        let dvips = run_geometry(
            &[0.12, 0.0, 0.0, -0.12, 100.0, 700.0],
            Some(300.0),
            0.0,
            12.0,
            true,
        );
        assert_eq!(dvips.rotation, 0.0);
        assert_eq!(dvips.y, 700.0);

        // A y-flip without that correction hangs the glyphs upside down: the
        // run is upside-down, its baseline at the top of its box.
        let flipped = run_geometry(
            &[1.0, 0.0, 0.0, -1.0, 100.0, 700.0],
            Some(36.0),
            0.0,
            12.0,
            false,
        );
        assert_eq!(flipped.rotation, 180.0);
        assert_eq!((flipped.y, flipped.height), (688.0, 12.0));

        // A mirrored run turned on its side reads up while its glyphs point
        // the other way: their orientation is what it reports.
        let sideways = run_geometry(
            &[0.0, 1.0, 1.0, 0.0, 100.0, 100.0],
            Some(36.0),
            0.0,
            12.0,
            false,
        );
        assert_eq!(sideways.rotation, 270.0);
    }

    #[test]
    fn run_geometry_reads_the_turn_from_the_font_size_sign_not_the_advance() {
        // A negative `Tf` size (negative `em`) negates the glyph matrix: the
        // run reads towards -x with its glyphs below the baseline, a 180°
        // turn; the advance it produced is negative too.
        let turned = run_geometry(
            &[1.0, 0.0, 0.0, 1.0, 100.0, 700.0],
            Some(-36.0),
            0.0,
            -12.0,
            false,
        );
        assert_eq!(turned.rotation, 180.0);
        assert_eq!((turned.x, turned.width), (64.0, 36.0));
        assert_eq!((turned.y, turned.height), (688.0, 12.0));
        assert!(turned.advance_known);

        // A negative advance alone — character spacing tighter than the
        // glyphs, or TJ positioning walking back — leaves the glyphs upright
        // on their baseline; only the box extends the other way.
        let tight = run_geometry(
            &[1.0, 0.0, 0.0, 1.0, 100.0, 700.0],
            Some(-36.0),
            0.0,
            12.0,
            false,
        );
        assert_eq!(tight.rotation, 0.0);
        assert_eq!((tight.x, tight.width), (64.0, 36.0));
        assert_eq!((tight.y, tight.height), (700.0, 12.0));
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
