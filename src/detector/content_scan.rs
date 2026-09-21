//! The executed-content scan behind the invisible-text-layer signal: a
//! byte-level walk of a page's content streams and of the Form XObjects
//! they invoke, following the graphics state a renderer would — the text
//! render mode, the transformation matrix, `q`/`Q`, a form's `/Matrix`
//! and `/BBox` — to tell the text-showing operators that leave nothing to
//! see from those that paint, and to tally the page area the images drawn
//! cover. The classification rule itself lives in the parent module.

use super::content_mask::{
    mask_strings_comments_and_inline_images, name_operand_before, numeric_operands_before,
    show_operand_has_text,
};
use super::{collect_text_chars_before, extract_font_name_before_tf, preceding_operand_closer};
use crate::extractor::{get_number, visible_page_box, PageBox};
use lopdf::{Document, Object, ObjectId};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// The share of the page area the images a page's content draws must
/// cover, their boxes clipped to the page, for the page to count as
/// covered.
const COVERING_IMAGE_MIN_PAGE_FRACTION: f64 = 0.5;

/// What a page's content executed, for the classification rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ExecutedContent {
    /// Text-showing operators executed: in the page's own content and in
    /// the Form XObjects that content invokes.
    pub(super) text_ops: u32,
    /// Those of `text_ops` that left nothing to see: run under text render
    /// mode 3, or under mode 7 with nothing painted through the clip.
    pub(super) hidden_text_ops: u32,
    /// Whether an image draw — an image XObject, an inline image or a
    /// path painted with a tiling pattern that draws one — landed within
    /// the clip in force: the page shows an image, whatever its resources
    /// bind.
    pub(super) draws_image: bool,
    /// Whether the images drawn cover at least
    /// `COVERING_IMAGE_MIN_PAGE_FRACTION` of the visible page box.
    pub(super) covers_page: bool,
    /// Whether every text-showing operator executed left nothing to see
    /// while the images drawn cover the page — and nothing went unread
    /// that could say otherwise.
    pub(super) shows_only_a_hidden_text_layer: bool,
}

/// The page's content streams read as one — the text render mode and the
/// matrix carry from each to the next — and followed through `Do`: an
/// image is measured on the visible page box, a form is run in place.
/// Names resolve in the page's own resources first, then in those it
/// inherits. Returns what the page executed and the streams' own counts;
/// the text characters and font names met go to the sets given.
pub(super) fn scan_page_content(
    doc: &Document,
    page_id: ObjectId,
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
) -> (ExecutedContent, ContentCounts) {
    let (state, counts) = scan_page(doc, page_id, unique_chars, used_font_names);
    (state.executed(), counts)
}

/// Whether the page shows only a text layer nobody sees over images that
/// cover it — from the executed-content scan alone, without the font,
/// pixel and path analysis of `analyze_page_content`. For the pages a
/// sample left out, whose OCR reason is reported all the same.
pub(super) fn page_shows_only_a_hidden_text_layer(doc: &Document, page_id: ObjectId) -> bool {
    scan_page(doc, page_id, &mut HashSet::new(), &mut HashSet::new())
        .0
        .shows_only_a_hidden_text_layer()
}

/// [`scan_content_stream`] of one stream on its own — the initial
/// graphics state, nothing followed through `Do` — as the counts alone,
/// which is all the walk over every form bound wants of it.
pub(super) fn scan_content_stream_alone(
    doc: &Document,
    content: &[u8],
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
) -> ContentCounts {
    let mut state = ContentScanState::new(doc, PageBox::LETTER, false);
    scan_content_stream(content, unique_chars, used_font_names, &mut state, &[])
}

/// The dictionaries a page's names resolve in, most specific first: its
/// own `/Resources`, then those it inherits from its ancestors (see
/// `resolve_with_shadowing`).
fn resource_chain<'a>(
    doc: &'a Document,
    own: Option<&'a lopdf::Dictionary>,
    ancestors: &[ObjectId],
) -> Vec<&'a lopdf::Dictionary> {
    own.into_iter()
        .chain(
            ancestors
                .iter()
                .filter_map(|id| doc.get_dictionary(*id).ok()),
        )
        .collect()
}

/// The scan of the page's content streams, followed through `Do`, and
/// the streams' own counts.
fn scan_page<'a>(
    doc: &'a Document,
    page_id: ObjectId,
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
) -> (ContentScanState<'a>, ContentCounts) {
    let page_box = visible_page_box(doc, page_id).unwrap_or(PageBox::LETTER);
    let page_resources = doc.get_page_resources(page_id).ok();
    let resources = page_resources
        .as_ref()
        .map(|(own, ancestors)| resource_chain(doc, *own, ancestors))
        .unwrap_or_default();
    let mut state = ContentScanState::new(doc, page_box, true);
    let mut counts = ContentCounts::default();
    for content_id in doc.get_page_contents(page_id) {
        if let Ok(Object::Stream(stream)) = doc.get_object(content_id) {
            let content = stream
                .decompressed_content()
                .unwrap_or_else(|_| stream.content.clone());
            counts.add(scan_content_stream(
                &content,
                unique_chars,
                used_font_names,
                &mut state,
                &resources,
            ));
        }
    }
    (state, counts)
}

/// What a scan of content streams counted.
#[derive(Clone, Copy, Default)]
pub(super) struct ContentCounts {
    /// Text-showing operators (`Tj`, `TJ`, `'`, `"`), whatever their render
    /// mode.
    pub(super) text_ops: u32,
    /// Image XObjects among the resources scanned, and inline images among
    /// the operators.
    pub(super) image_count: u32,
    /// Path construction and painting operators.
    pub(super) path_ops: u32,
    /// `Tf` operators whose font name could be read.
    pub(super) font_changes: u32,
}

impl ContentCounts {
    pub(super) fn add(&mut self, other: ContentCounts) {
        self.text_ops += other.text_ops;
        self.image_count += other.image_count;
        self.path_ops += other.path_ops;
        self.font_changes += other.font_changes;
    }
}

/// How many `q` levels the scan keeps a saved state for. Deeper nesting
/// keeps the innermost state, and its `Q`s restore nothing.
const SCAN_STATE_MAX_DEPTH: usize = 256;

/// How many form invocations one page's scan follows through `Do`. Past
/// it, content goes unread and the page's evidence is incomplete.
const FORM_INVOCATIONS_MAX: usize = 1_000;

/// How many decompressed bytes of form content one page's scan keeps for
/// forms invoked again.
const FORM_CONTENT_CACHE_MAX_BYTES: usize = 8 << 20;

/// Cells per side of the grid the images drawn are tallied on, over the
/// visible page box: 64 × 64 cells, one row per `u64`.
const COVERAGE_GRID: usize = 64;

/// A box in user space, `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct UserBox {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl UserBox {
    fn area(&self) -> f64 {
        (self.x1 - self.x0) * (self.y1 - self.y0)
    }

    /// The smallest box holding both.
    fn union(&self, other: &UserBox) -> UserBox {
        UserBox {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// `None` when the boxes do not overlap.
    fn intersect(&self, other: &UserBox) -> Option<UserBox> {
        let clipped = UserBox {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (clipped.x1 > clipped.x0 && clipped.y1 > clipped.y0).then_some(clipped)
    }
}

/// What `q` saves of the state the scan follows.
#[derive(Clone, Copy)]
struct SavedScanState {
    render_mode: u8,
    ctm: [f64; 6],
    clip: UserBox,
    fill_paints_image: bool,
    stroke_paints_image: bool,
    clip_text_ops: u32,
}

/// The part of the graphics state a content scan follows, saved by `q`
/// and restored by `Q`: the text render mode `Tr` sets; the current
/// transformation matrix `cm` concatenates; the clip the clipping paths
/// set with `W`/`W*` narrow — a rectangle exactly, any other shape by its
/// bounding box; whether the fill and stroke colours set with `scn`/`SCN`
/// are tiling patterns that draw an image; and the clip-only (mode 7)
/// text whose clip is in force, which a painting operator shows through
/// and the `Q` closing its level discards unseen.
///
/// One state runs through a page's content streams, which the PDF reads
/// as one, and — when it follows `Do` — through the Form XObjects they
/// invoke, at each invocation, with the state in force there, the form's
/// `/Matrix` applied, its `/BBox` clipping what it draws and the form's
/// own changes undone afterwards, as a renderer runs them; a form
/// invoking itself is not run again, and past `FORM_INVOCATIONS_MAX`
/// invocations the rest goes unread. It tallies what the page executes:
/// its text-showing operators, those of them that leave nothing to see,
/// and the page cells the images it draws cover. A scan that follows no
/// `Do` only counts.
struct ContentScanState<'a> {
    doc: &'a Document,
    /// The visible page box, which the coverage grid spans.
    page: UserBox,
    /// The clip in force for image draws: the page box, narrowed by the
    /// boxes of the forms being run and by the clipping paths set with
    /// `W`/`W*`.
    clip: UserBox,
    /// Whether `Do` is followed: images drawn are measured and forms are
    /// run in place.
    follow_do: bool,
    render_mode: u8,
    ctm: [f64; 6],
    /// The bounding box, in user space, of the path under construction.
    path_box: Option<UserBox>,
    /// Whether `W`/`W*` asked for the path under construction to become
    /// the clip once the operator ending the path has run.
    clip_pending: bool,
    /// Whether the fill colour, and the stroke colour, in force is a tiling
    /// pattern whose cell draws an image, so that a path painted with it
    /// is covered by an image.
    fill_paints_image: bool,
    stroke_paints_image: bool,
    /// Whether patterns named by `scn`/`SCN` are looked into — not within
    /// a pattern's own cell.
    follow_patterns: bool,
    /// Whether a pattern draws an image, per pattern looked into.
    pattern_verdicts: HashMap<ObjectId, bool>,
    /// Whether an image was drawn at all, wherever it fell.
    drew_image: bool,
    /// Whether an image draw landed within the clip in force.
    drew_image_on_page: bool,
    /// Mode-7 text-showing operators whose clip was set at the current
    /// level and has not been painted through; those set at outer levels
    /// sit in `saved`.
    clip_text_ops: u32,
    /// `clip_text_ops` over the current and the saved levels together.
    pending_clip_text_ops: u32,
    /// Mode-7 text-showing operators of the open text object: their clip
    /// takes effect at its `ET`, so what is painted before that — the rest
    /// of the text object — does not show through them.
    clip_text_ops_open: u32,
    saved: Vec<SavedScanState>,
    /// `q` operators past `SCAN_STATE_MAX_DEPTH`, whose `Q`s restore nothing.
    unsaved_depth: u32,
    /// The depth `Q` does not restore below: a form's content cannot close
    /// its invoker's levels.
    stack_floor: usize,
    /// Text-showing operators executed, all of them.
    executed_text_ops: u32,
    /// Those of `executed_text_ops` that left nothing to see: mode 3, or
    /// mode 7 with nothing painted through its clip.
    executed_hidden_text_ops: u32,
    /// The grid cells whose centres an image draw covered, one bit per
    /// cell, a row per word.
    covered_cells: [u64; COVERAGE_GRID],
    /// The images' own areas on the page added up, each no more than its
    /// box there — a turned image's box would overstate it.
    own_image_area: f64,
    /// The forms being run, innermost last.
    active_forms: Vec<ObjectId>,
    /// Form invocations followed so far, against `FORM_INVOCATIONS_MAX`.
    form_invocations: usize,
    /// Whether content went unread — the invocation budget or the depth
    /// cap ran out — so that the page's evidence is incomplete.
    incomplete: bool,
    /// Decompressed form content by object, for forms invoked again.
    form_content: HashMap<ObjectId, Rc<Vec<u8>>>,
    form_content_bytes: usize,
}

impl<'a> ContentScanState<'a> {
    fn new(doc: &'a Document, page_box: PageBox, follow_do: bool) -> Self {
        let page = UserBox {
            x0: f64::from(page_box.x0),
            y0: f64::from(page_box.y0),
            x1: f64::from(page_box.x1),
            y1: f64::from(page_box.y1),
        };
        Self {
            doc,
            page,
            clip: page,
            follow_do,
            render_mode: 0,
            ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            path_box: None,
            clip_pending: false,
            fill_paints_image: false,
            stroke_paints_image: false,
            follow_patterns: true,
            pattern_verdicts: HashMap::new(),
            drew_image: false,
            drew_image_on_page: false,
            clip_text_ops: 0,
            pending_clip_text_ops: 0,
            clip_text_ops_open: 0,
            saved: Vec::new(),
            unsaved_depth: 0,
            stack_floor: 0,
            executed_text_ops: 0,
            executed_hidden_text_ops: 0,
            covered_cells: [0; COVERAGE_GRID],
            own_image_area: 0.0,
            active_forms: Vec::new(),
            form_invocations: 0,
            incomplete: false,
            form_content: HashMap::new(),
            form_content_bytes: 0,
        }
    }

    fn save(&mut self) {
        if self.saved.len() < SCAN_STATE_MAX_DEPTH {
            self.saved.push(SavedScanState {
                render_mode: self.render_mode,
                ctm: self.ctm,
                clip: self.clip,
                fill_paints_image: self.fill_paints_image,
                stroke_paints_image: self.stroke_paints_image,
                clip_text_ops: self.clip_text_ops,
            });
            self.clip_text_ops = 0;
        } else {
            self.unsaved_depth += 1;
        }
    }

    /// A `Q` with nothing of its own saved is ignored, as renderers ignore
    /// it. The clips set at the level being left go with it: what they
    /// hid, with nothing painted through them, stays hidden.
    fn restore(&mut self) {
        if self.unsaved_depth > 0 {
            self.unsaved_depth -= 1;
        } else if self.saved.len() > self.stack_floor {
            if let Some(saved) = self.saved.pop() {
                self.pending_clip_text_ops -= self.clip_text_ops;
                self.render_mode = saved.render_mode;
                self.ctm = saved.ctm;
                self.clip = saved.clip;
                self.fill_paints_image = saved.fill_paints_image;
                self.stroke_paints_image = saved.stroke_paints_image;
                self.clip_text_ops = saved.clip_text_ops;
            }
        }
    }

    /// `cm`: the matrix given goes before the one in force.
    fn concat(&mut self, [a1, b1, c1, d1, e1, f1]: [f64; 6]) {
        let [a2, b2, c2, d2, e2, f2] = self.ctm;
        self.ctm = [
            a1 * a2 + b1 * c2,
            a1 * b2 + b1 * d2,
            c1 * a2 + d1 * c2,
            c1 * b2 + d1 * d2,
            e1 * a2 + f1 * c2 + e2,
            e1 * b2 + f1 * d2 + f2,
        ];
    }

    /// The bounding box of `[x0 y0 x1 y1]` under the matrix in force;
    /// `None` when it is not finite.
    fn transformed_box(&self, [x0, y0, x1, y1]: [f64; 4]) -> Option<UserBox> {
        let [a, b, c, d, e, f] = self.ctm;
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

    /// A text-showing operator ran. In mode 3 it left nothing to see; in
    /// mode 7 nothing yet — from the text object's `ET` on, its glyphs clip
    /// whatever is painted, until the `Q` closing its level; in any other
    /// mode it painted, through any clip in force.
    fn text_shown(&mut self) {
        self.executed_text_ops += 1;
        match self.render_mode {
            3 => self.executed_hidden_text_ops += 1,
            7 => {
                self.executed_hidden_text_ops += 1;
                self.clip_text_ops_open += 1;
            }
            _ => self.painted(),
        }
    }

    /// `ET`: the clip the text object's mode-7 text built takes effect.
    fn text_object_ended(&mut self) {
        self.clip_text_ops += self.clip_text_ops_open;
        self.pending_clip_text_ops += self.clip_text_ops_open;
        self.clip_text_ops_open = 0;
    }

    /// Something was painted, so the clip-only text in force shows it
    /// through its glyphs: that text is visible after all.
    fn painted(&mut self) {
        let shown = self.pending_clip_text_ops;
        if shown > 0 {
            self.executed_hidden_text_ops -= shown;
            self.pending_clip_text_ops = 0;
            self.clip_text_ops = 0;
            for saved in &mut self.saved {
                saved.clip_text_ops = 0;
            }
        }
    }

    /// A point of the path under construction, in user space.
    fn path_point(&mut self, x: f64, y: f64) {
        if let Some(point) = self.transformed_box([x, y, x, y]) {
            self.path_box = Some(self.path_box.map_or(point, |path| path.union(&point)));
        }
    }

    /// A rectangle of the path under construction (`re`), in user space.
    fn path_rect(&mut self, [x, y, w, h]: [f64; 4]) {
        if let Some(rect) = self.transformed_box([x, y, x + w, y + h]) {
            self.path_box = Some(self.path_box.map_or(rect, |path| path.union(&rect)));
        }
    }

    /// `W`/`W*`: the path under construction is to clip, once ended.
    fn clip_requested(&mut self) {
        self.clip_pending = true;
    }

    /// A painting operator or `n` ended the path. A clip asked for narrows
    /// the clip in force to the path's box — a rectangle exactly, a path
    /// of several rectangles or of any other shape by its bounding box —
    /// or to nothing when the path has no extent.
    fn path_ended(&mut self) {
        if self.clip_pending {
            self.clip = match self.path_box.and_then(|path| path.intersect(&self.clip)) {
                Some(clip) => clip,
                None => UserBox {
                    x0: self.clip.x0,
                    y0: self.clip.y0,
                    x1: self.clip.x0,
                    y1: self.clip.y0,
                },
            };
            self.clip_pending = false;
        }
        self.path_box = None;
    }

    /// A path was filled, or stroked, with a tiling pattern whose cell
    /// draws an image: the path's box within the clip in force is covered
    /// by an image, as near as the box comes to the path.
    fn path_painted_with_image(&mut self) {
        let Some(painted) = self.path_box.and_then(|path| path.intersect(&self.clip)) else {
            return;
        };
        self.drew_image = true;
        self.drew_image_on_page = true;
        self.own_image_area += painted.area();
        self.mark_cells(&painted);
    }

    /// Whether the clip in force has any extent for a paint to land in.
    fn clip_is_open(&self) -> bool {
        self.clip.area() > 0.0
    }

    /// Whether the path under construction reaches into the clip in force
    /// — a stroked line has no area, so touching is enough.
    fn path_touches_clip(&self) -> bool {
        self.clip_is_open()
            && self.path_box.is_some_and(|path| {
                path.x0 <= self.clip.x1
                    && path.x1 >= self.clip.x0
                    && path.y0 <= self.clip.y1
                    && path.y1 >= self.clip.y0
            })
    }

    /// Whether the pattern `name` names in the first of `resources`
    /// binding it is a tiling pattern (`/PatternType 1`) whose cell draws
    /// an image — an image XObject invoked, or an inline image — read once
    /// per pattern through the masked operator scan, with the cell's own
    /// resources and then its invoker's; a shading pattern, or a cell that
    /// draws no image, paints no coverage. Patterns are not looked into
    /// from within a pattern's cell, and the reading counts against the
    /// invocation budget, past which the evidence is incomplete.
    fn pattern_paints_image(&mut self, name: &[u8], resources: &[&'a lopdf::Dictionary]) -> bool {
        if !self.follow_patterns {
            return false;
        }
        let Some((id, pattern)) = resolve_pattern(self.doc, resources, name) else {
            return false;
        };
        if let Some(&verdict) = self.pattern_verdicts.get(&id) {
            return verdict;
        }
        if self.form_invocations >= FORM_INVOCATIONS_MAX {
            self.incomplete = true;
            return false;
        }
        self.form_invocations += 1;
        let verdict = match pattern {
            Object::Stream(cell) if pattern_type(self.doc, &cell.dict) == Some(1) => {
                let content = cell
                    .decompressed_content()
                    .unwrap_or_else(|_| cell.content.clone());
                let mut cell_resources = Vec::with_capacity(resources.len() + 1);
                cell_resources.extend(stream_resources(self.doc, cell));
                cell_resources.extend_from_slice(resources);
                let mut cell_state = ContentScanState::new(self.doc, PageBox::LETTER, true);
                cell_state.follow_patterns = false;
                scan_content_stream(
                    &content,
                    &mut HashSet::new(),
                    &mut HashSet::new(),
                    &mut cell_state,
                    &cell_resources,
                );
                self.form_invocations += cell_state.form_invocations;
                self.incomplete |= cell_state.incomplete;
                cell_state.drew_image
            }
            _ => false,
        };
        self.pattern_verdicts.insert(id, verdict);
        verdict
    }

    /// `Do` of an image, or `BI` of an inline image: it paints the unit
    /// square under the matrix in force, of which the part within the clip
    /// in force counts.
    fn image_drawn(&mut self) {
        self.drew_image = true;
        let Some(drawn) = self.transformed_box([0.0, 0.0, 1.0, 1.0]) else {
            return;
        };
        // Only a draw that lands within the clip in force paints anything:
        // an image off the page, or clipped away, shows no clip-only text
        // through it.
        let Some(on_page) = drawn.intersect(&self.clip) else {
            return;
        };
        self.painted();
        self.drew_image_on_page = true;
        let [a, b, c, d, _, _] = self.ctm;
        self.own_image_area += (a * d - b * c).abs().min(on_page.area());
        self.mark_cells(&on_page);
    }

    /// Mark the grid cells whose centres lie within `on_page`.
    fn mark_cells(&mut self, on_page: &UserBox) {
        let cells = COVERAGE_GRID as f64;
        let cell_w = (self.page.x1 - self.page.x0) / cells;
        let cell_h = (self.page.y1 - self.page.y0) / cells;
        let first_col = ((on_page.x0 - self.page.x0) / cell_w - 0.5).ceil().max(0.0);
        let last_col = ((on_page.x1 - self.page.x0) / cell_w - 0.5)
            .floor()
            .min(cells - 1.0);
        let first_row = ((on_page.y0 - self.page.y0) / cell_h - 0.5).ceil().max(0.0);
        let last_row = ((on_page.y1 - self.page.y0) / cell_h - 0.5)
            .floor()
            .min(cells - 1.0);
        if last_col < first_col || last_row < first_row {
            return;
        }
        let (first_col, last_col) = (first_col as usize, last_col as usize);
        let width = last_col - first_col + 1;
        let mask = if width >= COVERAGE_GRID {
            u64::MAX
        } else {
            ((1u64 << width) - 1) << first_col
        };
        for row in first_row as usize..=last_row as usize {
            self.covered_cells[row] |= mask;
        }
    }

    /// `Do` of a form: run in place, as a renderer runs it — under the
    /// state in force, with its `/Matrix` applied, its `/BBox` clipping
    /// what it draws, and its own changes undone afterwards. Its names
    /// resolve in its own resources first, then in its invoker's. A form
    /// whose box lies outside the clip in force shows nothing and is not
    /// read; a form invoking itself, directly or through others, is not
    /// run again; past the invocation budget or the depth cap the form
    /// goes unread and the page's evidence is incomplete.
    fn form_drawn(
        &mut self,
        id: ObjectId,
        form: &'a lopdf::Stream,
        invoker_resources: &[&'a lopdf::Dictionary],
    ) {
        if self.active_forms.contains(&id) {
            return;
        }
        if self.form_invocations >= FORM_INVOCATIONS_MAX || self.saved.len() >= SCAN_STATE_MAX_DEPTH
        {
            self.incomplete = true;
            return;
        }
        self.form_invocations += 1;

        let outer_floor = self.stack_floor;
        self.save();
        let base = self.saved.len();
        self.stack_floor = base;
        if let Some(matrix) = numbers_of(self.doc, &form.dict, b"Matrix") {
            self.concat(matrix);
        }
        let clip = match numbers_of(self.doc, &form.dict, b"BBox") {
            Some(bbox) => self
                .transformed_box(bbox)
                .and_then(|drawn| drawn.intersect(&self.clip)),
            None => Some(self.clip),
        };
        if let Some(clip) = clip {
            self.clip = clip;
            let content = self.form_content(id, form);
            let mut resources = Vec::with_capacity(invoker_resources.len() + 1);
            resources.extend(stream_resources(self.doc, form));
            resources.extend_from_slice(invoker_resources);
            self.active_forms.push(id);
            scan_content_stream(
                &content,
                &mut HashSet::new(),
                &mut HashSet::new(),
                self,
                &resources,
            );
            self.active_forms.pop();
        }
        // Levels the form left open close with it.
        while self.unsaved_depth > 0 || self.saved.len() > base {
            self.restore();
        }
        self.stack_floor = outer_floor;
        self.restore();
    }

    /// A form's content, decompressed once per page while the cache lasts.
    fn form_content(&mut self, id: ObjectId, form: &lopdf::Stream) -> Rc<Vec<u8>> {
        if let Some(content) = self.form_content.get(&id) {
            return Rc::clone(content);
        }
        let content = Rc::new(
            form.decompressed_content()
                .unwrap_or_else(|_| form.content.clone()),
        );
        if self.form_content_bytes + content.len() <= FORM_CONTENT_CACHE_MAX_BYTES {
            self.form_content_bytes += content.len();
            self.form_content.insert(id, Rc::clone(&content));
        }
        content
    }

    /// The page area the images drawn cover: the cells they covered, to
    /// the grid's resolution, and no more than their own areas add up to.
    fn covered_image_area(&self) -> f64 {
        let cells: u32 = self.covered_cells.iter().map(|row| row.count_ones()).sum();
        let cell_area = self.page.area() / (COVERAGE_GRID * COVERAGE_GRID) as f64;
        (f64::from(cells) * cell_area).min(self.own_image_area)
    }

    /// Whether the images drawn cover the page: at least
    /// `COVERING_IMAGE_MIN_PAGE_FRACTION` of its area.
    fn covers_page(&self) -> bool {
        self.covered_image_area() >= COVERING_IMAGE_MIN_PAGE_FRACTION * self.page.area()
    }

    /// Whether every text-showing operator executed left nothing to see
    /// while the images drawn cover the page — the raster is all the page
    /// shows, and the text layer describes it rather than being its
    /// content — and nothing went unread that could say otherwise.
    fn shows_only_a_hidden_text_layer(&self) -> bool {
        !self.incomplete
            && self.executed_text_ops > 0
            && self.executed_hidden_text_ops == self.executed_text_ops
            && self.covers_page()
    }

    /// What the content executed, for the classification rule.
    fn executed(&self) -> ExecutedContent {
        ExecutedContent {
            text_ops: self.executed_text_ops,
            hidden_text_ops: self.executed_hidden_text_ops,
            draws_image: self.drew_image_on_page,
            covers_page: self.covers_page(),
            shows_only_a_hidden_text_layer: self.shows_only_a_hidden_text_layer(),
        }
    }
}

/// What a `Do` operand names, in the first of the resources binding it.
enum XObjectDrawn<'a> {
    Image,
    Form(ObjectId, &'a lopdf::Stream),
}

fn resolve_xobject<'a>(
    doc: &'a Document,
    resources: &[&'a lopdf::Dictionary],
    name: &[u8],
) -> Option<XObjectDrawn<'a>> {
    for scope in resources {
        let xobjects = match scope.get(b"XObject").ok() {
            Some(Object::Dictionary(dict)) => dict,
            Some(Object::Reference(id)) => match doc.get_dictionary(*id) {
                Ok(dict) => dict,
                Err(_) => continue,
            },
            _ => continue,
        };
        let Ok(entry) = xobjects.get(name) else {
            continue;
        };
        let id = entry.as_reference().ok()?;
        let Ok(Object::Stream(stream)) = doc.get_object(id) else {
            return None;
        };
        // `/Subtype` may be held by reference; one that does not resolve
        // to a name leaves the stream neither image nor form, as the
        // resource walks leave it.
        let subtype = match stream.dict.get(b"Subtype").ok()? {
            Object::Name(name) => Some(name.as_slice()),
            Object::Reference(id) => doc.get_object(*id).ok().and_then(|o| o.as_name().ok()),
            _ => None,
        };
        return match subtype {
            Some(b"Image") => Some(XObjectDrawn::Image),
            Some(b"Form") => Some(XObjectDrawn::Form(id, stream)),
            _ => None,
        };
    }
    None
}

/// A stream's `/Resources`, inline or by reference.
fn stream_resources<'a>(
    doc: &'a Document,
    stream: &'a lopdf::Stream,
) -> Option<&'a lopdf::Dictionary> {
    match stream.dict.get(b"Resources").ok()? {
        Object::Dictionary(dict) => Some(dict),
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        _ => None,
    }
}

/// The pattern `name` names, in the first of `resources` binding it: a
/// stream for a tiling pattern, a dictionary for a shading pattern.
fn resolve_pattern<'a>(
    doc: &'a Document,
    resources: &[&'a lopdf::Dictionary],
    name: &[u8],
) -> Option<(ObjectId, &'a Object)> {
    for scope in resources {
        let patterns = match scope.get(b"Pattern").ok() {
            Some(Object::Dictionary(dict)) => dict,
            Some(Object::Reference(id)) => match doc.get_dictionary(*id) {
                Ok(dict) => dict,
                Err(_) => continue,
            },
            _ => continue,
        };
        let Ok(entry) = patterns.get(name) else {
            continue;
        };
        let id = entry.as_reference().ok()?;
        return doc.get_object(id).ok().map(|pattern| (id, pattern));
    }
    None
}

/// A pattern's `/PatternType`: 1 for tiling, 2 for shading.
fn pattern_type(doc: &Document, dict: &lopdf::Dictionary) -> Option<i64> {
    match dict.get(b"PatternType").ok()? {
        Object::Reference(id) => doc.get_object(*id).ok()?.as_i64().ok(),
        other => other.as_i64().ok(),
    }
}

/// The first `N` numbers of `dict`'s `key` — a form's `/Matrix` or
/// `/BBox` — the array and its numbers direct or by reference. An array
/// with more entries is read by its first `N`, as a page box with
/// trailing entries is; one with fewer, or with something other than a
/// number among the first `N`, gives nothing — the form then runs
/// unclipped, or under the identity, there being nothing to clip or
/// scale by.
fn numbers_of<const N: usize>(
    doc: &Document,
    dict: &lopdf::Dictionary,
    key: &[u8],
) -> Option<[f64; N]> {
    let array = match dict.get(key).ok()? {
        Object::Array(array) => array,
        Object::Reference(id) => doc.get_object(*id).ok()?.as_array().ok()?,
        _ => return None,
    };
    if array.len() < N {
        return None;
    }
    let mut numbers = [0.0f64; N];
    for (slot, value) in numbers.iter_mut().zip(array) {
        let number = match value {
            Object::Reference(id) => doc.get_object(*id).ok().and_then(get_number),
            other => get_number(other),
        }?;
        *slot = f64::from(number);
    }
    Some(numbers)
}

/// Fast scan of content stream bytes for text operators
///
/// This is a fast heuristic scan that looks for:
/// - "Tj" - show text string
/// - "TJ" - show text with individual glyph positioning
/// - "'" and "\"" - move to the next line and show text
/// - "Tf" - set font, whose name goes to `used_font_names`
/// - path construction and painting operators
///
/// Operators are found in a copy of the stream with its strings, comments
/// and inline image data blanked, so none of those can pass for one; the
/// text itself is read from the original. An operator ends at whitespace,
/// at the end of the stream or at an opening delimiter, so `(a)Tj(b)Tj`
/// counts two shows; a show operator whose operand holds no string byte
/// shows nothing and is not counted. The scan follows the graphics state
/// in `state`: `Tr` sets the text render mode, which decides whether a
/// text-showing operator left anything to see; `cm` concatenates the
/// matrix an image is drawn under; `W`/`W*` clip; the colour operators
/// say whether a path painted next is filled, or stroked, with a tiling
/// pattern that draws an image; `q` and `Q` save and restore all of it. A
/// `Do` — when the state follows them — draws the image, or runs the
/// form, that the first of `resources` binding its name holds. Unique
/// non-whitespace text characters are collected into `unique_chars`.
fn scan_content_stream<'a>(
    content: &[u8],
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
    state: &mut ContentScanState<'a>,
    resources: &[&'a lopdf::Dictionary],
) -> ContentCounts {
    let mut counts = ContentCounts::default();
    let masked = mask_strings_comments_and_inline_images(content);
    let ops: &[u8] = &masked;

    // Helper: check if position is a word boundary (start of content or preceded by whitespace)
    let is_word_start = |pos: usize| -> bool { pos == 0 || ops[pos - 1].is_ascii_whitespace() };
    // A token starts after whitespace or a closing delimiter (`(a)Tj`,
    // `[<41>]TJ`, `Q/Im0 Do`) and ends before whitespace, the end of the
    // stream or an opening delimiter (`Tf[`, `Tj(`, `cm/Im0`).
    let is_token_start = |pos: usize| -> bool {
        pos == 0 || ops[pos - 1].is_ascii_whitespace() || matches!(ops[pos - 1], b')' | b']' | b'>')
    };
    let is_token_end = |pos: usize| -> bool {
        pos + 1 >= ops.len()
            || ops[pos + 1].is_ascii_whitespace()
            || matches!(ops[pos + 1], b'/' | b'[' | b'(' | b'<' | b'%')
    };
    // Whether the operator `token` sits at `pos`, on token boundaries.
    let token_at = |pos: usize, token: &[u8]| -> bool {
        ops[pos..].starts_with(token) && is_token_start(pos) && is_token_end(pos + token.len() - 1)
    };

    // Simple state machine to find operators.
    // Each lookback stops at the previous operator so a malformed `] TJ`
    // (no `[`) cannot rescan the entire prefix — that was quadratic in the
    // number of operators.
    // `Tj`/`TJ` are only counted when the preceding token closes a string or
    // array (')', '>', ']').
    let mut operand_floor = 0usize;
    let mut i = 0;
    while i < ops.len() {
        let b = ops[i];

        // Look for 'T' followed by 'j', 'J', 'f' or 'r'
        if b == b'T' && i + 1 < ops.len() {
            let next = ops[i + 1];
            if (next == b'j' || next == b'J')
                && is_token_end(i + 1)
                && preceding_operand_closer(ops, i, operand_floor)
            {
                if show_operand_has_text(ops, content, i, operand_floor) {
                    counts.text_ops += 1;
                    state.text_shown();
                    collect_text_chars_before(content, i, unique_chars, operand_floor);
                }
                operand_floor = i;
            } else if next == b'f' && is_token_end(i + 1) {
                // Tf = set font operator. Some PDFs concatenate Tf with the
                // next operator without whitespace (e.g. "25 Tf[<01>..." or
                // "25 Tf(<text>...").
                if let Some(name) = extract_font_name_before_tf(ops, i, operand_floor) {
                    used_font_names.insert(name);
                    counts.font_changes += 1;
                    operand_floor = i;
                }
            } else if next == b'r' && is_token_start(i) && is_token_end(i + 1) {
                // Tr = set text render mode. A mode outside 0..=7 is
                // ignored, as renderers ignore it.
                if let Some([mode]) = numeric_operands_before::<1>(ops, i, operand_floor) {
                    if mode.fract() == 0.0 && (0.0..=7.0).contains(&mode) {
                        state.render_mode = mode as u8;
                    }
                    operand_floor = i;
                }
            }
        } else if (b == b'\'' || b == b'"')
            && is_token_start(i)
            && is_token_end(i)
            && preceding_operand_closer(ops, i, operand_floor)
        {
            // ' and " = move to the next line and show text (" sets the
            // word and character spacing first). An apostrophe inside a
            // string was blanked, so it cannot get here.
            if show_operand_has_text(ops, content, i, operand_floor) {
                counts.text_ops += 1;
                state.text_shown();
                collect_text_chars_before(content, i, unique_chars, operand_floor);
            }
            operand_floor = i;
        } else if token_at(i, b"cm") {
            // cm = concatenate matrix.
            if let Some(matrix) = numeric_operands_before::<6>(ops, i, operand_floor) {
                state.concat(matrix);
                operand_floor = i;
            }
        } else if token_at(i, b"Do") {
            // Do = paint an XObject: an image is measured, a form run in
            // place. Whether a page has images at all is read from its
            // resources (scan_xobjects_in_resources, analyze_page_images).
            if let Some(name) = name_operand_before(ops, i, operand_floor) {
                operand_floor = i;
                if state.follow_do {
                    match resolve_xobject(state.doc, resources, &name) {
                        Some(XObjectDrawn::Image) => state.image_drawn(),
                        Some(XObjectDrawn::Form(id, form)) => state.form_drawn(id, form, resources),
                        None => {}
                    }
                }
            }
        } else if token_at(i, b"sh") {
            // sh = paint a shading, over the clip in force — nothing when
            // the clip has no extent.
            if state.clip_is_open() {
                state.painted();
            }
        } else if token_at(i, b"BI") {
            // BI = begin an inline image, which paints the unit square
            // under the matrix in force as an image XObject does, and is
            // an image of the page as one bound in its resources is.
            counts.image_count += 1;
            state.image_drawn();
        } else if token_at(i, b"ET") {
            // ET = end a text object: its clip-only text's clip takes effect.
            state.text_object_ended();
        } else if token_at(i, b"scn") || token_at(i, b"sc") {
            // scn/sc = set the fill colour. A name names a pattern, which
            // draws an image or does not; numbers name none.
            let paints_image = name_operand_before(ops, i, operand_floor)
                .is_some_and(|name| state.pattern_paints_image(&name, resources));
            state.fill_paints_image = paints_image;
            operand_floor = i;
        } else if token_at(i, b"SCN") || token_at(i, b"SC") {
            // SCN/SC = set the stroke colour, likewise.
            let paints_image = name_operand_before(ops, i, operand_floor)
                .is_some_and(|name| state.pattern_paints_image(&name, resources));
            state.stroke_paints_image = paints_image;
            operand_floor = i;
        } else if token_at(i, b"cs") || token_at(i, b"g") || token_at(i, b"rg") || token_at(i, b"k")
        {
            // A fill colour space or a plain fill colour: no pattern fills.
            state.fill_paints_image = false;
        } else if token_at(i, b"CS") || token_at(i, b"G") || token_at(i, b"RG") || token_at(i, b"K")
        {
            state.stroke_paints_image = false;
        } else if token_at(i, b"q") {
            state.save();
        } else if token_at(i, b"Q") {
            state.restore();
        }

        // Count path construction/painting operators.
        // Single-byte: m (moveto), l (lineto), c (curveto), h (closepath),
        //              f (fill), S (stroke), s (close+stroke), B (fill+stroke),
        //              F (fill, variant)
        // These are the high-volume operators in vector-outlined text.
        // A painting operator also shows any clip-only text through its
        // glyphs, and paints an image over the path's box when the colour
        // it paints with is a tiling pattern that draws one; b, B* and b*
        // paint too but are not counted as path operators. The path's
        // points are followed for its box; v and y add points without
        // being counted either.
        let mut painted = false;
        let mut path_ended = false;
        let mut fills = false;
        let mut strokes = false;
        match b {
            b'm' | b'l' if is_word_start(i) && is_token_end(i) => {
                counts.path_ops += 1;
                if let Some([x, y]) = numeric_operands_before::<2>(ops, i, operand_floor) {
                    state.path_point(x, y);
                }
            }
            b'c' if is_word_start(i) && is_token_end(i) => {
                counts.path_ops += 1;
                if let Some([x1, y1, x2, y2, x3, y3]) =
                    numeric_operands_before::<6>(ops, i, operand_floor)
                {
                    state.path_point(x1, y1);
                    state.path_point(x2, y2);
                    state.path_point(x3, y3);
                }
            }
            b'v' | b'y' if is_word_start(i) && is_token_end(i) => {
                if let Some([x1, y1, x2, y2]) = numeric_operands_before::<4>(ops, i, operand_floor)
                {
                    state.path_point(x1, y1);
                    state.path_point(x2, y2);
                }
            }
            b'h' if is_word_start(i) && is_token_end(i) => {
                counts.path_ops += 1;
            }
            b'f' | b'F' if is_word_start(i) && is_token_end(i) => {
                counts.path_ops += 1;
                painted = true;
                path_ended = true;
                fills = true;
            }
            b'S' | b's' if is_word_start(i) && is_token_end(i) => {
                counts.path_ops += 1;
                painted = true;
                path_ended = true;
                strokes = true;
            }
            b'B' if is_word_start(i) && is_token_end(i) => {
                counts.path_ops += 1;
                painted = true;
                path_ended = true;
                fills = true;
                strokes = true;
            }
            b'b' if is_word_start(i) && is_token_end(i) => {
                painted = true;
                path_ended = true;
                fills = true;
                strokes = true;
            }
            b'n' if is_word_start(i) && is_token_end(i) => {
                path_ended = true;
            }
            b'W' if is_word_start(i) && is_token_end(i) => {
                state.clip_requested();
            }
            // Two-byte: re (rect), f* (fill even-odd), B*/b*, W* (clip even-odd)
            b'r' if ops.get(i + 1) == Some(&b'e') && is_word_start(i) && is_token_end(i + 1) => {
                counts.path_ops += 1;
                if let Some(rect) = numeric_operands_before::<4>(ops, i, operand_floor) {
                    state.path_rect(rect);
                }
            }
            b'f' if ops.get(i + 1) == Some(&b'*') && is_word_start(i) && is_token_end(i + 1) => {
                counts.path_ops += 1;
                painted = true;
                path_ended = true;
                fills = true;
            }
            b'B' | b'b'
                if ops.get(i + 1) == Some(&b'*') && is_word_start(i) && is_token_end(i + 1) =>
            {
                painted = true;
                path_ended = true;
                fills = true;
                strokes = true;
            }
            b'W' if ops.get(i + 1) == Some(&b'*') && is_word_start(i) && is_token_end(i + 1) => {
                state.clip_requested();
            }
            _ => {}
        }
        // A path painted off the page, or clipped away, paints nothing.
        if painted && state.path_touches_clip() {
            state.painted();
        }
        if (fills && state.fill_paints_image) || (strokes && state.stroke_paints_image) {
            state.path_painted_with_image();
        }
        if path_ended {
            state.path_ended();
        }

        i += 1;
    }

    counts
}

#[cfg(test)]
#[path = "content_scan_tests.rs"]
mod tests;
