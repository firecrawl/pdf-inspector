//! The executed-content scan behind the invisible-text-layer signal: a
//! byte-level walk of a page's content streams and of the Form XObjects
//! they invoke, following the graphics state a renderer would — the text
//! render mode, the transformation matrix, `q`/`Q`, a form's `/Matrix`
//! and `/BBox`, and where the text-positioning operators put the text
//! shown — to tell the text-showing operators that leave nothing to see
//! from those that paint, and to tally the page area the images drawn
//! cover. The classification rule itself lives in the parent module.

use super::content_geometry::{box_under, multiply, ClipText, Reach, TextPosition, UserBox};
use super::content_mask::{
    mask_strings_comments_and_inline_images, name_operand_before, numeric_operands_before,
    show_operand_text_bytes,
};
use super::content_resources::{
    colour_space_bound, colour_space_components, components_of_colour_space, decoded_within,
    numbers_of, pattern_type, resolve_pattern, resolve_xobject, stream_resources, XObjectDrawn,
};
use super::{
    collect_text_chars_before, extract_font_name_before_tf, is_pdf_whitespace,
    preceding_operand_closer,
};
use crate::extractor::{visible_page_box, PageBox};
use lopdf::{Document, Object, ObjectId};
use std::borrow::Cow;
use std::cell::Cell;
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
    /// mode 3, or under mode 7 with nothing painted where its glyphs lie.
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
    /// Bytes of form and pattern-cell content executed.
    pub(super) form_bytes: usize,
    /// Whether a form, or a pattern's cell, went unread because its
    /// content would take the bytes executed past `EXECUTED_FORM_BYTES_MAX`,
    /// or is itself past `FORM_CONTENT_CACHE_MAX_BYTES` — from which point
    /// no form is followed and the evidence is incomplete.
    pub(super) form_bytes_exceeded: bool,
}

/// The page's content streams read as one — the text render mode and the
/// matrix carry from each to the next — and followed through `Do`: an
/// image is measured on the visible page box, a form is run in place.
/// Names resolve in the page's own resources first, then in those it
/// inherits. Returns what the page executed and the streams' own counts;
/// the text characters and font names met go to the sets given. The
/// forms' runs come from, and go to, the document's `form_scans`.
pub(super) fn scan_page_content<'a>(
    doc: &'a Document,
    page_id: ObjectId,
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
    form_scans: &mut FormScans<'a>,
) -> (ExecutedContent, ContentCounts) {
    let (state, counts) = scan_page(doc, page_id, unique_chars, used_font_names, form_scans);
    (state.executed(), counts)
}

/// What the page's content executed — from the executed-content scan
/// alone, without the font, pixel and path analysis of
/// `analyze_page_content`. For the pages a sample left out, whose OCR
/// reason is reported all the same.
pub(super) fn page_executed_content<'a>(
    doc: &'a Document,
    page_id: ObjectId,
    form_scans: &mut FormScans<'a>,
) -> ExecutedContent {
    scan_page(
        doc,
        page_id,
        &mut HashSet::new(),
        &mut HashSet::new(),
        form_scans,
    )
    .0
    .executed()
}

/// [`scan_content_stream`] of one stream on its own — the initial
/// graphics state, nothing followed through `Do`, no pattern looked into
/// — as the counts alone, which is all the walk over every form bound
/// wants of it. `resources` are the stream's own, for the colour spaces
/// its inline images name.
pub(super) fn scan_content_stream_alone<'a>(
    doc: &'a Document,
    content: &[u8],
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
    resources: &[&'a lopdf::Dictionary],
) -> ContentCounts {
    let mut state = ContentScanState::new(doc, PageBox::LETTER, false);
    state.follow_patterns = false;
    scan_content_stream(
        content,
        unique_chars,
        used_font_names,
        &mut state,
        resources,
    )
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
    form_scans: &mut FormScans<'a>,
) -> (ContentScanState<'a>, ContentCounts) {
    let page_box = visible_page_box(doc, page_id).unwrap_or(PageBox::LETTER);
    let page_resources = doc.get_page_resources(page_id).ok();
    let resources = page_resources
        .as_ref()
        .map(|(own, ancestors)| resource_chain(doc, *own, ancestors))
        .unwrap_or_default();
    let mut state = ContentScanState::new(doc, page_box, true);
    state.form_runs = std::mem::take(&mut form_scans.runs);
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
    form_scans.runs = std::mem::take(&mut state.form_runs);
    (state, counts)
}

/// What a scan of content streams counted.
#[derive(Clone, Copy, Default)]
pub(super) struct ContentCounts {
    /// Text-showing operators (`Tj`, `TJ`, `'`, `"`), whatever their render
    /// mode.
    pub(super) text_ops: u32,
    /// Image XObjects among the resources scanned, and — in an executed
    /// scan — inline images among the operators.
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
/// keeps the innermost state, its `Q`s restore nothing, and the page's
/// evidence is incomplete: a render mode or clip set past the cap would
/// outlive its `Q`.
const SCAN_STATE_MAX_DEPTH: usize = 256;

/// How many form invocations one page's scan follows through `Do`. Past
/// it, content goes unread and the page's evidence is incomplete.
const FORM_INVOCATIONS_MAX: usize = 1_000;

/// How many bytes of form and pattern-cell content one page's scan
/// executes — the invocations alone would let a large form be run into
/// hundreds of megabytes. From the form that would pass it on, no form is
/// followed and the page's evidence is incomplete.
const EXECUTED_FORM_BYTES_MAX: usize = 32 << 20;

/// The byte budget a scan runs under: `EXECUTED_FORM_BYTES_MAX`.
#[cfg(not(test))]
fn executed_form_bytes_budget() -> usize {
    EXECUTED_FORM_BYTES_MAX
}

/// The byte budget a scan runs under: what the test on this thread set in
/// place of `EXECUTED_FORM_BYTES_MAX`, to reach the budget without content
/// of that size, or the constant.
#[cfg(test)]
fn executed_form_bytes_budget() -> usize {
    EXECUTED_FORM_BYTES_OVERRIDE
        .with(std::cell::Cell::get)
        .unwrap_or(EXECUTED_FORM_BYTES_MAX)
}

#[cfg(test)]
thread_local! {
    /// The byte budget set in place of `EXECUTED_FORM_BYTES_MAX` on this thread.
    static EXECUTED_FORM_BYTES_OVERRIDE: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// How many bytes of form content — decompressed, and its masked copy —
/// one page's scan keeps for forms invoked again; and how many one form's
/// or cell's stream may decode to, whatever the page's byte budget has
/// left: a stream past it is not read.
const FORM_CONTENT_CACHE_MAX_BYTES: usize = 8 << 20;

/// How many text objects' clip-only text one `q` level keeps apart for
/// the test of what is painted where their glyphs lie. Past it, further
/// text objects join the last, under the union of their boxes — coarser,
/// never less revealing.
const CLIP_TEXT_ENTRIES_MAX: usize = 256;

/// The identity matrix.
const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Cells per side of the grid the images drawn are tallied on, over the
/// visible page box: 64 × 64 cells, one row per `u64`.
const COVERAGE_GRID: usize = 64;

/// How many form runs one document's scans keep for replay (see
/// [`FormRuns`]), and how many of them one form keeps under one state.
const FORM_RUNS_KEPT_MAX: usize = 4_096;
const FORM_RUNS_PER_STATE_MAX: usize = 8;

/// How many images a run may draw and still be kept: a replay adds their
/// areas one at a time, in the order the run added them.
const FORM_RUN_IMAGE_AREAS_MAX: usize = 256;

/// What one document's scans learn of its forms, kept from page to page:
/// a form that page after page binds, or draws, is read once rather than
/// once a page — as the walk over the forms a page binds reads it, and as
/// the executed scan runs it, under each state it is drawn in.
#[derive(Default)]
pub(super) struct FormScans<'a> {
    runs: FormRuns<'a>,
    walks: HashMap<ObjectId, FormWalk>,
}

/// How many forms' walks one document's scans keep (see [`FormWalk`]):
/// past it, a form is walked again on each page that binds it.
const FORM_WALKS_KEPT_MAX: usize = 4_096;

/// What a form's content holds, read on its own — as
/// [`scan_content_stream_alone`] reads it, in the form's own resources —
/// which is the form's alone, whichever page binds it.
#[derive(Clone)]
pub(super) struct FormWalk {
    pub(super) counts: ContentCounts,
    pub(super) unique_chars: HashSet<u8>,
    pub(super) font_names: HashSet<Vec<u8>>,
}

impl FormWalk {
    fn read(doc: &Document, form: &lopdf::Stream) -> Self {
        let content = form
            .decompressed_content()
            .unwrap_or_else(|_| form.content.clone());
        let own: Vec<&lopdf::Dictionary> = stream_resources(doc, form).into_iter().collect();
        let mut unique_chars = HashSet::new();
        let mut font_names = HashSet::new();
        let counts =
            scan_content_stream_alone(doc, &content, &mut unique_chars, &mut font_names, &own);
        Self {
            counts,
            unique_chars,
            font_names,
        }
    }
}

impl FormScans<'_> {
    /// The [`FormWalk`] of the form `id`: read the first time it is asked
    /// for, and kept while fewer than `FORM_WALKS_KEPT_MAX` are.
    pub(super) fn form_walk(
        &mut self,
        doc: &Document,
        id: ObjectId,
        form: &lopdf::Stream,
    ) -> Cow<'_, FormWalk> {
        if !self.walks.contains_key(&id) {
            let walk = FormWalk::read(doc, form);
            if self.walks.len() >= FORM_WALKS_KEPT_MAX {
                return Cow::Owned(walk);
            }
            self.walks.insert(id, walk);
        }
        Cow::Borrowed(&self.walks[&id])
    }
}

/// Form runs kept for replay: a form drawn again under a state it ran
/// under before adds what it added then, without running again. That is
/// what running it again would add when the run began and ended with
/// nothing pending that it could see or leave — no clip-only text in
/// force or in an open text object, no text object positioned, no path
/// under construction — went complete, and looked up no pattern, whose
/// verdicts the page keeps once tested; and when the replay is at no
/// greater depth, within budgets that admit what the run took, and —
/// when a name the run looked up resolved in its invoker's resources, or
/// in none — under an invoker whose resources are the same.
pub(super) struct FormRuns<'a> {
    by_state: HashMap<FormRunKey, Vec<FormRun<'a>>>,
    kept: usize,
    limit: usize,
    #[cfg(test)]
    replays: usize,
}

impl Default for FormRuns<'_> {
    fn default() -> Self {
        Self {
            by_state: HashMap::new(),
            kept: 0,
            limit: FORM_RUNS_KEPT_MAX,
            #[cfg(test)]
            replays: 0,
        }
    }
}

#[cfg(test)]
impl FormRuns<'_> {
    /// Runs that keep none: every form drawn is run.
    pub(super) fn none() -> Self {
        Self {
            limit: 0,
            ..Self::default()
        }
    }
}

impl<'a> FormRuns<'a> {
    fn is_full(&self) -> bool {
        self.kept >= self.limit
    }

    fn keep(&mut self, key: FormRunKey, run: FormRun<'a>) {
        if self.is_full() {
            return;
        }
        let runs = self.by_state.entry(key).or_default();
        if runs.len() < FORM_RUNS_PER_STATE_MAX {
            runs.push(run);
            self.kept += 1;
        }
    }
}

/// The form, and the state in force where it is drawn, that a run reads:
/// its numbers by their bits, so that a state that differs in any of them
/// is another state.
#[derive(PartialEq, Eq, Hash)]
struct FormRunKey {
    form: ObjectId,
    active_forms: Vec<ObjectId>,
    page: [u64; 4],
    clip: [u64; 4],
    ctm: [u64; 6],
    font_size: Option<u64>,
    leading: u64,
    render_mode: u8,
    fill_paints_image: bool,
    stroke_paints_image: bool,
}

/// What one run of a form added to the scan's tallies.
struct FormRun<'a> {
    /// The invoker's resources, when a name the run looked up resolved in
    /// them, or in none: another invoker's may bind it otherwise.
    invoker_resources: Option<Vec<&'a lopdf::Dictionary>>,
    /// The run's `resolved_from_end` less the invoker's scopes, for a run
    /// recorded around a replay of this one; `None` when it looked up no
    /// name.
    resolved_past_invoker: Option<isize>,
    /// The depth the run began at: complete there, it runs the same at any
    /// depth up to it.
    depth: usize,
    invocations: usize,
    form_bytes: usize,
    text_ops: u32,
    hidden_text_ops: u32,
    drew_image: bool,
    drew_image_on_page: bool,
    covered_cells: [u64; COVERAGE_GRID],
    image_areas: Vec<f64>,
}

/// The tallies as a recorded run began, and those the page had: the
/// latter are set aside for the run's own, and joined again after it.
struct FormRunRecording {
    depth: usize,
    invocations: usize,
    form_bytes: usize,
    text_ops: u32,
    hidden_text_ops: u32,
    pattern_lookups: usize,
    outer_incomplete: bool,
    outer_drew_image: bool,
    outer_drew_image_on_page: bool,
    outer_covered_cells: [u64; COVERAGE_GRID],
    outer_image_areas: Option<Vec<f64>>,
    outer_resolved_from_end: usize,
}

/// Whether two resource chains bind the same: the same dictionaries, or
/// dictionaries of the same entries.
fn same_resources(a: &[&lopdf::Dictionary], b: &[&lopdf::Dictionary]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| std::ptr::eq(*a, *b) || a == b)
}

fn box_bits(user_box: &UserBox) -> [u64; 4] {
    [
        user_box.x0.to_bits(),
        user_box.y0.to_bits(),
        user_box.x1.to_bits(),
        user_box.y1.to_bits(),
    ]
}

/// A form's content as the scan reads it: decompressed, with the masked
/// copy the operators are read through — kept for the page when the
/// masking owed nothing to the invoker's resources (see `form_content`).
struct FormContent {
    content: Vec<u8>,
    masked: Vec<u8>,
}

/// What `q` saves of the state the scan follows.
struct SavedScanState {
    render_mode: u8,
    ctm: [f64; 6],
    clip: UserBox,
    fill_paints_image: bool,
    stroke_paints_image: bool,
    font_size: Option<f64>,
    leading: f64,
    clip_text: Vec<ClipText>,
}

/// The part of the graphics state a content scan follows, saved by `q`
/// and restored by `Q`: the text render mode `Tr` sets; the current
/// transformation matrix `cm` concatenates; the clip the clipping paths
/// set with `W`/`W*` narrow — a rectangle exactly, any other shape by its
/// bounding box; whether the fill and stroke colours set with `scn`/`SCN`
/// are tiling patterns that draw an image; the font size `Tf` sets and
/// the leading `TL`/`TD` set, which with the text-positioning operators
/// place the glyphs shown; and the clip-only (mode 7) text whose clip is
/// in force — each text object of it with the box its glyphs lie in —
/// which a painting operator landing on that box shows through, and the
/// `Q` closing its level discards unseen.
///
/// One state runs through a page's content streams, which the PDF reads
/// as one, and — when it follows `Do` — through the Form XObjects they
/// invoke, at each invocation, with the state in force there, the form's
/// `/Matrix` applied, its `/BBox` clipping what it draws and the form's
/// own changes undone afterwards, as a renderer runs them; a form
/// invoking itself is not run again, and past `FORM_INVOCATIONS_MAX`
/// invocations, or `EXECUTED_FORM_BYTES_MAX` bytes of form content, the
/// rest goes unread. It tallies what the page executes:
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
    /// Whether a pattern draws an image, per pattern looked into and per
    /// resources in force where it was used — the forms being run at the
    /// time, in whose resources its cell's names may resolve.
    pattern_verdicts: HashMap<(ObjectId, Vec<ObjectId>), bool>,
    /// Whether an image was drawn at all, wherever it fell.
    drew_image: bool,
    /// Whether an image draw landed within the clip in force.
    drew_image_on_page: bool,
    /// The clip-only text whose clip was set at the current level and has
    /// not been painted through, a text object at a time; that of outer
    /// levels sits in `saved`.
    clip_text: Vec<ClipText>,
    /// The text-showing operators of `clip_text` over the current and the
    /// saved levels together.
    pending_clip_text_ops: u32,
    /// Mode-7 text-showing operators of the open text object, and where
    /// their glyphs lie: their clip takes effect at its `ET`, so what is
    /// painted before that — the rest of the text object — does not show
    /// through them.
    clip_text_ops_open: u32,
    clip_text_reach_open: Reach,
    /// The font size `Tf` set; `None` until one is.
    font_size: Option<f64>,
    /// The leading `TL` or `TD` set, which `T*`, `'` and `"` move by.
    leading: f64,
    /// Where the open text object shows next, once a text-positioning
    /// operator has said; `None` until then, and outside text objects.
    text_position: Option<TextPosition>,
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
    /// The forms being run, innermost last — through whose resources the
    /// names in force resolve.
    active_forms: Vec<ObjectId>,
    /// Form invocations followed so far, against `FORM_INVOCATIONS_MAX`.
    form_invocations: usize,
    /// Bytes of form and pattern-cell content executed so far, against
    /// `form_bytes_budget` — `executed_form_bytes_budget()` when the scan
    /// began.
    executed_form_bytes: usize,
    form_bytes_budget: usize,
    /// Whether a form or a cell went unread for the byte budget — its
    /// content would have taken the bytes executed past it, or is itself
    /// past `FORM_CONTENT_CACHE_MAX_BYTES`; from then on none is followed.
    form_bytes_exceeded: bool,
    /// Whether content went unread — the invocation budget, the byte
    /// budget or the depth cap ran out — so that the page's evidence is
    /// incomplete.
    incomplete: bool,
    /// Form content by object — decompressed, and masked — for forms
    /// invoked again.
    form_content: HashMap<ObjectId, Rc<FormContent>>,
    form_content_bytes: usize,
    /// Form runs kept for replay — the document's, for the scan of one of
    /// its pages (see [`FormRuns`]).
    form_runs: FormRuns<'a>,
    /// Patterns looked up: a recorded run that looked one up is not kept.
    pattern_lookups: usize,
    /// While a run is recorded, the areas added to `own_image_area`, in
    /// order.
    image_area_log: Option<Vec<f64>>,
    /// While a run is recorded, how near the end of its resource chain
    /// the names looked up resolved: the fewest scopes from the end, the
    /// one binding the name included — a name bound nowhere counts as
    /// bound in the last. Every chain a run looks names up in ends with
    /// its invoker's resources, so the run owes them nothing when this
    /// exceeds their count.
    resolved_from_end: usize,
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
            clip_text: Vec::new(),
            pending_clip_text_ops: 0,
            clip_text_ops_open: 0,
            clip_text_reach_open: Reach::Nowhere,
            font_size: None,
            leading: 0.0,
            text_position: None,
            saved: Vec::new(),
            unsaved_depth: 0,
            stack_floor: 0,
            executed_text_ops: 0,
            executed_hidden_text_ops: 0,
            covered_cells: [0; COVERAGE_GRID],
            own_image_area: 0.0,
            active_forms: Vec::new(),
            form_invocations: 0,
            executed_form_bytes: 0,
            form_bytes_budget: executed_form_bytes_budget(),
            form_bytes_exceeded: false,
            incomplete: false,
            form_content: HashMap::new(),
            form_content_bytes: 0,
            form_runs: FormRuns::default(),
            pattern_lookups: 0,
            image_area_log: None,
            resolved_from_end: usize::MAX,
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
                font_size: self.font_size,
                leading: self.leading,
                clip_text: std::mem::take(&mut self.clip_text),
            });
        } else {
            self.unsaved_depth += 1;
            self.incomplete = true;
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
                let discarded: u32 = self.clip_text.iter().map(|text| text.ops).sum();
                self.pending_clip_text_ops -= discarded;
                self.render_mode = saved.render_mode;
                self.ctm = saved.ctm;
                self.clip = saved.clip;
                self.fill_paints_image = saved.fill_paints_image;
                self.stroke_paints_image = saved.stroke_paints_image;
                self.font_size = saved.font_size;
                self.leading = saved.leading;
                self.clip_text = saved.clip_text;
            }
        }
    }

    /// `cm`: the matrix given goes before the one in force.
    fn concat(&mut self, matrix: [f64; 6]) {
        self.ctm = multiply(matrix, self.ctm);
    }

    /// The bounding box of `[x0 y0 x1 y1]` under the matrix in force;
    /// `None` when it is not finite.
    fn transformed_box(&self, corners: [f64; 4]) -> Option<UserBox> {
        box_under(self.ctm, corners)
    }

    /// `BT`: a text object opens, its position yet to be set.
    fn text_object_began(&mut self) {
        self.text_position = None;
    }

    /// `Td`/`TD`: the next line starts `tx`, `ty` from the start of the
    /// current one — from the text space's origin when nothing has
    /// positioned the text object yet.
    fn text_moved(&mut self, tx: f64, ty: f64) {
        let from = self
            .text_position
            .map_or(IDENTITY, |position| position.line);
        let line = multiply([1.0, 0.0, 0.0, 1.0, tx, ty], from);
        self.text_position = Some(TextPosition { matrix: line, line });
    }

    /// `Tm`: the text matrix, and the line matrix, are set outright.
    fn text_matrix_set(&mut self, matrix: [f64; 6]) {
        self.text_position = Some(TextPosition {
            matrix,
            line: matrix,
        });
    }

    /// `T*`, and the move `'` and `"` make first: the next line, a leading
    /// down.
    fn next_line(&mut self) {
        self.text_moved(0.0, -self.leading);
    }

    /// The box, in user space, of `bytes` bytes of text shown at the pen:
    /// half an em per byte long, an em tall above the baseline and a
    /// quarter em below it, under the text matrix and the matrix in
    /// force. This is an overlap test, not layout: character and word
    /// spacing, horizontal scaling, rise and the adjustments in a `TJ`
    /// array are not followed. `None` when the text object has not been
    /// positioned or no font size has been set: the text is then placed
    /// nowhere the scan can tell.
    fn glyph_box(&self, bytes: usize) -> Option<UserBox> {
        let size = self.font_size?.abs();
        let position = self.text_position?;
        let width = 0.5 * size * bytes as f64;
        box_under(
            multiply(position.matrix, self.ctm),
            [0.0, -0.25 * size, width, size],
        )
    }

    /// The pen moves past `bytes` bytes shown, half an em each, along the
    /// baseline.
    fn pen_advanced(&mut self, bytes: usize) {
        let Some(size) = self.font_size else {
            return;
        };
        let Some(position) = self.text_position.as_mut() else {
            return;
        };
        let width = 0.5 * size.abs() * bytes as f64;
        let [a, b, ..] = position.matrix;
        position.matrix[4] += width * a;
        position.matrix[5] += width * b;
    }

    /// A text-showing operator ran, over `bytes` bytes of text. In mode 3
    /// it left nothing to see; in mode 7 nothing yet — from the text
    /// object's `ET` on, its glyphs clip whatever is painted where they
    /// lie, until the `Q` closing its level; in any other mode it painted
    /// its glyphs, through any clip in force. Text placed nowhere the
    /// scan can tell is taken to reach wherever paint lands, and to paint
    /// wherever clip-only text lies — the rule before text was placed at
    /// all.
    fn text_shown(&mut self, bytes: usize) {
        self.executed_text_ops += 1;
        match self.render_mode {
            3 => self.executed_hidden_text_ops += 1,
            7 => {
                self.executed_hidden_text_ops += 1;
                self.clip_text_ops_open += 1;
                let reach = match self.glyph_box(bytes) {
                    None => Reach::Anywhere,
                    Some(glyphs) => glyphs
                        .intersect(&self.clip)
                        .map_or(Reach::Nowhere, Reach::Within),
                };
                self.clip_text_reach_open = self.clip_text_reach_open.join(reach);
            }
            // With no clip-only text in force there is nothing for the
            // paint to show, and its box is not needed.
            _ if self.pending_clip_text_ops > 0 => match self.glyph_box(bytes) {
                None => self.painted(None),
                Some(glyphs) => {
                    if let Some(landed) = glyphs.intersect(&self.clip) {
                        self.painted(Some(landed));
                    }
                }
            },
            _ => {}
        }
        self.pen_advanced(bytes);
    }

    /// `ET`: the clip the text object's mode-7 text built takes effect,
    /// where its glyphs lie. Past `CLIP_TEXT_ENTRIES_MAX` text objects at
    /// one level, further ones join the last, under the union of their
    /// boxes.
    fn text_object_ended(&mut self) {
        self.text_position = None;
        if self.clip_text_ops_open == 0 {
            return;
        }
        let text = ClipText {
            ops: self.clip_text_ops_open,
            reach: self.clip_text_reach_open,
        };
        self.clip_text_ops_open = 0;
        self.clip_text_reach_open = Reach::Nowhere;
        self.pending_clip_text_ops += text.ops;
        let full = self.clip_text.len() >= CLIP_TEXT_ENTRIES_MAX;
        match self.clip_text.last_mut() {
            Some(last) if full => {
                last.ops += text.ops;
                last.reach = last.reach.join(text.reach);
            }
            _ => self.clip_text.push(text),
        }
    }

    /// Something was painted on `landed` — `None` when its extent is not
    /// known — so the clip-only text in force whose glyphs it landed on
    /// shows it through them: that text is visible after all.
    fn painted(&mut self, landed: Option<UserBox>) {
        if self.pending_clip_text_ops == 0 {
            return;
        }
        fn reveal(texts: &mut Vec<ClipText>, landed: Option<UserBox>) -> u32 {
            let mut shown = 0;
            texts.retain(|text| {
                let seen = text.reach.shown_by(landed);
                if seen {
                    shown += text.ops;
                }
                !seen
            });
            shown
        }
        let mut shown = reveal(&mut self.clip_text, landed);
        for saved in &mut self.saved {
            shown += reveal(&mut saved.clip_text, landed);
        }
        self.executed_hidden_text_ops -= shown;
        self.pending_clip_text_ops -= shown;
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
        self.add_image_area(painted.area());
        self.mark_cells(&painted);
    }

    /// An image's own area on the page joins `own_image_area` — and the
    /// log of a run being recorded, for its replays to add in turn.
    fn add_image_area(&mut self, area: f64) {
        self.own_image_area += area;
        if let Some(log) = self.image_area_log.as_mut() {
            log.push(area);
        }
    }

    /// Whether the clip in force has any extent for a paint to land in.
    fn clip_is_open(&self) -> bool {
        self.clip.area() > 0.0
    }

    /// Where the path under construction lands within the clip in force
    /// — a stroked line has no area, so touching is enough; `None` when
    /// it misses the clip, or the clip has no extent.
    fn path_landed(&self) -> Option<UserBox> {
        if !self.clip_is_open() {
            return None;
        }
        self.path_box?.clamped(&self.clip)
    }

    /// Whether the pattern `name` names in the first of `resources`
    /// binding it is a tiling pattern (`/PatternType 1`) whose cell draws
    /// an image — an image XObject invoked, or an inline image — read
    /// through the masked operator scan, with the cell's own resources
    /// and then its invoker's, once per pattern and per resources in
    /// force: a cell whose own resources do not bind a name draws what
    /// its invoker's bind, which one form's may differently from
    /// another's. A shading pattern, or a cell that draws no image,
    /// paints no coverage. Patterns are not looked into from within a
    /// pattern's cell, and the reading counts against the invocation and
    /// byte budgets, past which the evidence is incomplete.
    fn pattern_paints_image(&mut self, name: &[u8], resources: &[&'a lopdf::Dictionary]) -> bool {
        if !self.follow_patterns {
            return false;
        }
        self.pattern_lookups += 1;
        let Some((id, pattern)) = resolve_pattern(self.doc, resources, name) else {
            return false;
        };
        let scope = (id, self.active_forms.clone());
        if let Some(&verdict) = self.pattern_verdicts.get(&scope) {
            return verdict;
        }
        if self.form_invocations >= FORM_INVOCATIONS_MAX {
            self.incomplete = true;
            return false;
        }
        self.form_invocations += 1;
        let verdict = match pattern {
            Object::Stream(cell) if pattern_type(self.doc, &cell.dict) == Some(1) => {
                let Some(content) = self.form_bytes_admitted(cell) else {
                    return false;
                };
                let mut cell_resources = Vec::with_capacity(resources.len() + 1);
                cell_resources.extend(stream_resources(self.doc, cell));
                cell_resources.extend_from_slice(resources);
                let mut cell_state = ContentScanState::new(self.doc, PageBox::LETTER, true);
                cell_state.follow_patterns = false;
                // The forms the cell invokes run against the page's budgets.
                cell_state.executed_form_bytes = self.executed_form_bytes;
                cell_state.form_bytes_budget = self.form_bytes_budget;
                scan_content_stream(
                    &content,
                    &mut HashSet::new(),
                    &mut HashSet::new(),
                    &mut cell_state,
                    &cell_resources,
                );
                self.form_invocations += cell_state.form_invocations;
                self.executed_form_bytes = cell_state.executed_form_bytes;
                self.form_bytes_exceeded |= cell_state.form_bytes_exceeded;
                self.incomplete |= cell_state.incomplete;
                cell_state.drew_image
            }
            _ => false,
        };
        self.pattern_verdicts.insert(scope, verdict);
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
        self.painted(Some(on_page));
        self.drew_image_on_page = true;
        let [a, b, c, d, _, _] = self.ctm;
        self.add_image_area((a * d - b * c).abs().min(on_page.area()));
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
    /// run again; past the invocation budget or the depth cap, or with
    /// content the byte budget does not admit — which is not decoded past
    /// what it admits — the form goes unread and the page's evidence is
    /// incomplete. A run kept under the same state is replayed instead
    /// (see [`FormRuns`]).
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

        // Drawn under a state it ran under before, the form adds what it
        // added then; run, it is recorded when nothing pending can make
        // another run of it differ.
        let key = self.at_rest().then(|| self.form_run_key(id));
        if let Some(key) = &key {
            if self.replay_form_run(key, invoker_resources) {
                return;
            }
        }
        let recording = match key {
            Some(key) if !self.form_runs.is_full() => Some((key, self.begin_recording())),
            _ => None,
        };
        self.run_form(id, form, invoker_resources);
        if let Some((key, recording)) = recording {
            self.end_recording(key, recording, invoker_resources);
        }
    }

    /// Whether nothing is pending that a form's run could see, reveal or
    /// leave changed — no clip-only text in force or in an open text
    /// object, no text object positioned, no path under construction —
    /// in a scan that follows patterns and has not run out of bytes.
    fn at_rest(&self) -> bool {
        self.follow_patterns
            && !self.form_bytes_exceeded
            && self.pending_clip_text_ops == 0
            && self.clip_text_ops_open == 0
            && self.text_position.is_none()
            && self.path_box.is_none()
            && !self.clip_pending
    }

    fn form_run_key(&self, form: ObjectId) -> FormRunKey {
        FormRunKey {
            form,
            active_forms: self.active_forms.clone(),
            page: box_bits(&self.page),
            clip: box_bits(&self.clip),
            ctm: self.ctm.map(f64::to_bits),
            font_size: self.font_size.map(f64::to_bits),
            leading: self.leading.to_bits(),
            render_mode: self.render_mode,
            fill_paints_image: self.fill_paints_image,
            stroke_paints_image: self.stroke_paints_image,
        }
    }

    /// Adds what a run kept under `key` added, when there is one this
    /// invocation would run the same (see [`FormRuns`]); whether there was.
    fn replay_form_run(
        &mut self,
        key: &FormRunKey,
        invoker_resources: &[&'a lopdf::Dictionary],
    ) -> bool {
        let runs = std::mem::take(&mut self.form_runs);
        let depth = self.saved.len();
        let run = runs.by_state.get(key).and_then(|runs| {
            runs.iter().find(|run| {
                depth <= run.depth
                    && self.form_invocations + run.invocations <= FORM_INVOCATIONS_MAX
                    && self.executed_form_bytes + run.form_bytes <= self.form_bytes_budget
                    && run
                        .invoker_resources
                        .as_deref()
                        .is_none_or(|kept| same_resources(kept, invoker_resources))
            })
        });
        if let Some(run) = run {
            self.form_invocations += run.invocations;
            self.executed_form_bytes += run.form_bytes;
            self.executed_text_ops += run.text_ops;
            self.executed_hidden_text_ops += run.hidden_text_ops;
            self.drew_image |= run.drew_image;
            self.drew_image_on_page |= run.drew_image_on_page;
            for (cells, run_cells) in self.covered_cells.iter_mut().zip(&run.covered_cells) {
                *cells |= run_cells;
            }
            for &area in &run.image_areas {
                self.add_image_area(area);
            }
            if let Some(past) = run.resolved_past_invoker {
                let from_end = (invoker_resources.len() as isize + past).max(1) as usize;
                self.resolved_from_end = self.resolved_from_end.min(from_end);
            }
        }
        let replayed = run.is_some();
        self.form_runs = runs;
        #[cfg(test)]
        if replayed {
            self.form_runs.replays += 1;
        }
        replayed
    }

    /// Sets the page's tallies aside for the run about to be recorded.
    fn begin_recording(&mut self) -> FormRunRecording {
        FormRunRecording {
            depth: self.saved.len(),
            invocations: self.form_invocations,
            form_bytes: self.executed_form_bytes,
            text_ops: self.executed_text_ops,
            hidden_text_ops: self.executed_hidden_text_ops,
            pattern_lookups: self.pattern_lookups,
            outer_incomplete: std::mem::take(&mut self.incomplete),
            outer_drew_image: std::mem::take(&mut self.drew_image),
            outer_drew_image_on_page: std::mem::take(&mut self.drew_image_on_page),
            outer_covered_cells: std::mem::replace(&mut self.covered_cells, [0; COVERAGE_GRID]),
            outer_image_areas: self.image_area_log.replace(Vec::new()),
            outer_resolved_from_end: std::mem::replace(&mut self.resolved_from_end, usize::MAX),
        }
    }

    /// Keeps the recorded run when a replay of it adds what running it
    /// again would (see [`FormRuns`]), and joins its tallies to the
    /// page's.
    fn end_recording(
        &mut self,
        key: FormRunKey,
        recording: FormRunRecording,
        invoker_resources: &[&'a lopdf::Dictionary],
    ) {
        let image_areas = std::mem::replace(&mut self.image_area_log, recording.outer_image_areas)
            .unwrap_or_default();
        if let Some(log) = self.image_area_log.as_mut() {
            log.extend_from_slice(&image_areas);
        }
        let resolved_from_end = self.resolved_from_end;
        let kept = !self.incomplete
            && self.pattern_lookups == recording.pattern_lookups
            && self.at_rest()
            && image_areas.len() <= FORM_RUN_IMAGE_AREAS_MAX;
        if kept {
            let invoker_scopes = invoker_resources.len();
            let run = FormRun {
                invoker_resources: (resolved_from_end <= invoker_scopes)
                    .then(|| invoker_resources.to_vec()),
                resolved_past_invoker: (resolved_from_end != usize::MAX)
                    .then(|| resolved_from_end as isize - invoker_scopes as isize),
                depth: recording.depth,
                invocations: self.form_invocations - recording.invocations,
                form_bytes: self.executed_form_bytes - recording.form_bytes,
                text_ops: self.executed_text_ops - recording.text_ops,
                hidden_text_ops: self.executed_hidden_text_ops - recording.hidden_text_ops,
                drew_image: self.drew_image,
                drew_image_on_page: self.drew_image_on_page,
                covered_cells: self.covered_cells,
                image_areas,
            };
            self.form_runs.keep(key, run);
        }
        self.incomplete |= recording.outer_incomplete;
        self.drew_image |= recording.outer_drew_image;
        self.drew_image_on_page |= recording.outer_drew_image_on_page;
        for (cells, outer) in self
            .covered_cells
            .iter_mut()
            .zip(recording.outer_covered_cells)
        {
            *cells |= outer;
        }
        self.resolved_from_end = resolved_from_end.min(recording.outer_resolved_from_end);
    }

    /// A name looked up in a chain of `scopes` resolved in the one at
    /// `index` — in none when `None`, as though in the last.
    fn name_resolved(&mut self, scopes: usize, index: Option<usize>) {
        let from_end = index.map_or(1, |index| scopes - index);
        self.resolved_from_end = self.resolved_from_end.min(from_end);
    }

    /// The run [`Self::form_drawn`] makes of a form.
    fn run_form(
        &mut self,
        id: ObjectId,
        form: &'a lopdf::Stream,
        invoker_resources: &[&'a lopdf::Dictionary],
    ) {
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
            let mut resources = Vec::with_capacity(invoker_resources.len() + 1);
            resources.extend(stream_resources(self.doc, form));
            resources.extend_from_slice(invoker_resources);
            if let Some(content) = self.form_content(id, form, &resources) {
                self.active_forms.push(id);
                scan_masked_content(
                    &content.content,
                    &content.masked,
                    &mut HashSet::new(),
                    &mut HashSet::new(),
                    self,
                    &resources,
                );
                self.active_forms.pop();
            }
        }
        // Levels the form left open close with it.
        while self.unsaved_depth > 0 || self.saved.len() > base {
            self.restore();
        }
        self.stack_floor = outer_floor;
        self.restore();
    }

    /// A form's content, charged to the byte budget at each invocation:
    /// admitted to it (see `form_bytes_admitted`) and masked — the colour
    /// spaces its inline images name resolved in `resources`, those in
    /// force at the invocation, the form's own first. It is kept for the
    /// page while the cache lasts, both copies counting against the
    /// cache's budget, only when the form's own resources bound every
    /// colour space the masking asked after — a space they bind is the
    /// form's reading, of a family known here or not: a name they leave to
    /// the invoker's resources may be bound otherwise under another
    /// invoker, and with it the length of the data and where the operators
    /// resume change, so such a form is masked at each invocation. `None`
    /// when the byte budget refuses it.
    fn form_content(
        &mut self,
        id: ObjectId,
        form: &lopdf::Stream,
        resources: &[&'a lopdf::Dictionary],
    ) -> Option<Rc<FormContent>> {
        if let Some(content) = self.form_content.get(&id) {
            let content = Rc::clone(content);
            return self
                .form_bytes_charged(content.content.len())
                .then_some(content);
        }
        let content = self.form_bytes_admitted(form)?;
        let doc = self.doc;
        let own = stream_resources(doc, form);
        let asked_the_invoker = Cell::new(false);
        let resolved_from_end = Cell::new(usize::MAX);
        let masked = mask_strings_comments_and_inline_images(&content, &|name| {
            match own.and_then(|scope| colour_space_bound(doc, scope, name)) {
                // Bound by the form's own resources, the space reads as
                // they have it — of no family known here, as unknown —
                // as the first binding decides everywhere else.
                Some(space) => components_of_colour_space(doc, space, 0),
                None => {
                    asked_the_invoker.set(true);
                    let bound = resources.iter().enumerate().find_map(|(index, scope)| {
                        colour_space_bound(doc, scope, name).map(|space| (index, space))
                    });
                    let from_end = bound.map_or(1, |(index, _)| resources.len() - index);
                    resolved_from_end.set(resolved_from_end.get().min(from_end));
                    bound.and_then(|(_, space)| components_of_colour_space(doc, space, 0))
                }
            }
        });
        self.resolved_from_end = self.resolved_from_end.min(resolved_from_end.get());
        let content = Rc::new(FormContent { content, masked });
        let bytes = content.content.len() + content.masked.len();
        if !asked_the_invoker.get()
            && self.form_content_bytes + bytes <= FORM_CONTENT_CACHE_MAX_BYTES
        {
            self.form_content_bytes += bytes;
            self.form_content.insert(id, Rc::clone(&content));
        }
        Some(content)
    }

    /// A form's or a cell's content admitted to the byte budget and
    /// charged to it. The stream is decoded no further than the budget's
    /// remainder — nothing once any was refused — nor than
    /// `FORM_CONTENT_CACHE_MAX_BYTES`, so that no more than either is ever
    /// held: a raw stream is refused by its length before any copy, a
    /// filtered one by the bounded decoder at the limit. `None` when it is
    /// refused: nothing further is followed, and the page's evidence is
    /// incomplete.
    fn form_bytes_admitted(&mut self, stream: &lopdf::Stream) -> Option<Vec<u8>> {
        if self.form_bytes_exceeded {
            self.form_bytes_refused();
            return None;
        }
        let limit = self
            .form_bytes_budget
            .saturating_sub(self.executed_form_bytes)
            .min(FORM_CONTENT_CACHE_MAX_BYTES);
        let Some(content) = decoded_within(stream, limit) else {
            self.form_bytes_refused();
            return None;
        };
        self.executed_form_bytes += content.len();
        Some(content)
    }

    /// Whether `bytes` more of form content already decoded may be
    /// executed within the byte budget, charging them when they may. When
    /// they may not — or once any were refused — the form is refused.
    fn form_bytes_charged(&mut self, bytes: usize) -> bool {
        if self.form_bytes_exceeded || self.executed_form_bytes + bytes > self.form_bytes_budget {
            self.form_bytes_refused();
            return false;
        }
        self.executed_form_bytes += bytes;
        true
    }

    /// A form or a cell is refused for the byte budget: nothing further is
    /// followed, and the page's evidence is incomplete.
    fn form_bytes_refused(&mut self) {
        self.form_bytes_exceeded = true;
        self.incomplete = true;
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
            form_bytes: self.executed_form_bytes,
            form_bytes_exceeded: self.form_bytes_exceeded,
        }
    }
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
/// pattern that draws an image; `Tf`, `Tm`, `Td`, `TD`, `T*` and `TL`
/// place the text shown, for where clip-only text lies; `q` and `Q` save
/// and restore all of it. A `Do` — when the state follows them — draws
/// the image, or runs the form, that the first of `resources` binding its
/// name holds. Unique non-whitespace text characters are collected into
/// `unique_chars`.
fn scan_content_stream<'a>(
    content: &[u8],
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
    state: &mut ContentScanState<'a>,
    resources: &[&'a lopdf::Dictionary],
) -> ContentCounts {
    let doc = state.doc;
    let masked = mask_strings_comments_and_inline_images(content, &|name| {
        colour_space_components(doc, resources, name)
    });
    scan_masked_content(
        content,
        &masked,
        unique_chars,
        used_font_names,
        state,
        resources,
    )
}

/// [`scan_content_stream`] over `content` and `masked`, its masked copy —
/// made once for a form however often it is invoked.
fn scan_masked_content<'a>(
    content: &[u8],
    masked: &[u8],
    unique_chars: &mut HashSet<u8>,
    used_font_names: &mut HashSet<Vec<u8>>,
    state: &mut ContentScanState<'a>,
    resources: &[&'a lopdf::Dictionary],
) -> ContentCounts {
    let mut counts = ContentCounts::default();
    let ops: &[u8] = masked;

    // Helper: check if position is a word boundary (start of content or
    // preceded by whitespace — the file format's, NUL included).
    let is_word_start = |pos: usize| -> bool { pos == 0 || is_pdf_whitespace(ops[pos - 1]) };
    // A token starts after whitespace or a closing delimiter (`(a)Tj`,
    // `[<41>]TJ`, `Q/Im0 Do`) and ends before whitespace, the end of the
    // stream or an opening delimiter (`Tf[`, `Tj(`, `cm/Im0`).
    let is_token_start = |pos: usize| -> bool {
        pos == 0 || is_pdf_whitespace(ops[pos - 1]) || matches!(ops[pos - 1], b')' | b']' | b'>')
    };
    let is_token_end = |pos: usize| -> bool {
        pos + 1 >= ops.len()
            || is_pdf_whitespace(ops[pos + 1])
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
                let bytes = show_operand_text_bytes(ops, content, i, operand_floor);
                if bytes > 0 {
                    counts.text_ops += 1;
                    state.text_shown(bytes);
                    collect_text_chars_before(content, i, unique_chars, operand_floor);
                }
                operand_floor = i;
            } else if next == b'f' && is_token_end(i + 1) {
                // Tf = set font and size. Some PDFs concatenate Tf with the
                // next operator without whitespace (e.g. "25 Tf[<01>..." or
                // "25 Tf(<text>...").
                if let Some([size]) = numeric_operands_before::<1>(ops, i, operand_floor) {
                    state.font_size = Some(size);
                }
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
            } else if (next == b'd' || next == b'D') && is_token_start(i) && is_token_end(i + 1) {
                // Td/TD = move to the start of the next line, offset from
                // the start of the current one; TD sets the leading as well.
                if let Some([tx, ty]) = numeric_operands_before::<2>(ops, i, operand_floor) {
                    if next == b'D' {
                        state.leading = -ty;
                    }
                    state.text_moved(tx, ty);
                    operand_floor = i;
                }
            } else if next == b'm' && is_token_start(i) && is_token_end(i + 1) {
                // Tm = set the text matrix and the line matrix.
                if let Some(matrix) = numeric_operands_before::<6>(ops, i, operand_floor) {
                    state.text_matrix_set(matrix);
                    operand_floor = i;
                }
            } else if next == b'*' && is_token_start(i) && is_token_end(i + 1) {
                // T* = move to the start of the next line.
                state.next_line();
            } else if next == b'L' && is_token_start(i) && is_token_end(i + 1) {
                // TL = set the leading.
                if let Some([leading]) = numeric_operands_before::<1>(ops, i, operand_floor) {
                    state.leading = leading;
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
            state.next_line();
            let bytes = show_operand_text_bytes(ops, content, i, operand_floor);
            if bytes > 0 {
                counts.text_ops += 1;
                state.text_shown(bytes);
                collect_text_chars_before(content, i, unique_chars, operand_floor);
            }
            operand_floor = i;
        } else if matches!(
            b,
            b'c' | b'D'
                | b's'
                | b'B'
                | b'E'
                | b'S'
                | b'g'
                | b'r'
                | b'k'
                | b'C'
                | b'G'
                | b'R'
                | b'K'
                | b'q'
                | b'Q'
        ) {
            // The operators below, met by their first byte: a byte that
            // begins none of them — most bytes — is done with here.
            if token_at(i, b"cm") {
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
                        let drawn = resolve_xobject(state.doc, resources, &name);
                        state.name_resolved(resources.len(), drawn.as_ref().map(|(at, _)| *at));
                        match drawn {
                            Some((_, XObjectDrawn::Image)) => state.image_drawn(),
                            Some((_, XObjectDrawn::Form(id, form))) => {
                                state.form_drawn(id, form, resources)
                            }
                            None => {}
                        }
                    }
                }
            } else if token_at(i, b"sh") {
                // sh = paint a shading, over the clip in force — nothing when
                // the clip has no extent.
                if state.clip_is_open() {
                    state.painted(Some(state.clip));
                }
            } else if token_at(i, b"BI") {
                // BI = begin an inline image, which paints the unit square
                // under the matrix in force as an image XObject does. It counts
                // among the page's images, and is measured, only in an
                // executed scan: the walk over every bound form keeps its
                // tally of bound image XObjects, which an inline image in a
                // form never invoked is not, and reads nothing of its state.
                if state.follow_do {
                    counts.image_count += 1;
                    state.image_drawn();
                }
            } else if token_at(i, b"BT") {
                // BT = begin a text object, which the operators to come position.
                state.text_object_began();
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
            } else if token_at(i, b"cs")
                || token_at(i, b"g")
                || token_at(i, b"rg")
                || token_at(i, b"k")
            {
                // A fill colour space or a plain fill colour: no pattern fills.
                state.fill_paints_image = false;
            } else if token_at(i, b"CS")
                || token_at(i, b"G")
                || token_at(i, b"RG")
                || token_at(i, b"K")
            {
                state.stroke_paints_image = false;
            } else if token_at(i, b"q") {
                state.save();
            } else if token_at(i, b"Q") {
                state.restore();
            }
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
        let landed = if painted { state.path_landed() } else { None };
        if let Some(landed) = landed {
            state.painted(Some(landed));
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
#[path = "content_scan_budget_tests.rs"]
mod budget_tests;
#[cfg(test)]
#[path = "content_scan_clip_tests.rs"]
mod clip_tests;
#[cfg(test)]
#[path = "content_scan_fixtures.rs"]
mod fixtures;
#[cfg(test)]
#[path = "content_scan_replay_tests.rs"]
mod replay_tests;
#[cfg(test)]
#[path = "content_scan_tests.rs"]
mod tests;
