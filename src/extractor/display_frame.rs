//! The rendered frame of a page: the visible page box turned clockwise by
//! the page's inheritable `/Rotate`, the way renderers draw it.
//!
//! Positioned items leave the crate in the *sheet* frame — the visible page
//! box as laid out in the content stream, `/Rotate` not applied, with a page
//! whose text is predominantly rotated turned into a synthetic landscape
//! frame (see [`PageRotation`]) — and the region readers take their rects in
//! the same frame. A caller pairing the crate with a page renderer sees the
//! page turned by `/Rotate` instead. [`PositionFrame::Display`] reports items
//! in, and reads regions from, that rendered frame.

use std::collections::HashMap;

use lopdf::{Document, Object, ObjectId};

use super::geometry::{normalize_degrees, PageRotation};
use super::page_box::{visible_page_box, PageBox};
use crate::types::{ItemType, TextItem};

/// Coordinate frame positioned items are reported in and region rects are
/// read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PositionFrame {
    /// The visible page box (`CropBox ∩ MediaBox`, else the MediaBox) as laid
    /// out in the content stream, `/Rotate` not applied. Items use the box's
    /// lower-left corner as origin with `y` growing upward; regions use its
    /// top-left corner with `y` growing downward. A page whose text is
    /// predominantly rotated is turned so that text reads left-to-right (see
    /// [`PageRotation`]), and its items live in that turned frame.
    #[default]
    Sheet,
    /// The rendered page: the visible page box turned clockwise by the
    /// page's inheritable `/Rotate`, with the same origin conventions as
    /// [`PositionFrame::Sheet`]. The turn of a predominantly rotated page is
    /// undone first, so every item sits where a renderer draws it and a
    /// region rect taken from a rendered page image selects the text under
    /// it.
    Display,
}

/// A page's `/Rotate`, normalised to a right angle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PageRotate {
    #[default]
    Rotate0,
    Rotate90,
    Rotate180,
    Rotate270,
}

impl PageRotate {
    /// Snap an angle to a right angle in `[0, 360)` the way renderers do:
    /// negatives fold (`-90` → `270`), oversized values wrap (`450` → `90`),
    /// and anything else rounds to the nearest right angle. A non-finite
    /// value reads as `0`.
    pub(crate) fn from_degrees(degrees: f64) -> PageRotate {
        if !degrees.is_finite() {
            return PageRotate::Rotate0;
        }
        let folded = degrees.rem_euclid(360.0);
        match (((folded + 45.0) / 90.0).floor() as i64).rem_euclid(4) {
            0 => PageRotate::Rotate0,
            1 => PageRotate::Rotate90,
            2 => PageRotate::Rotate180,
            _ => PageRotate::Rotate270,
        }
    }

    /// The clockwise turn a renderer applies to the sheet, in degrees.
    pub(crate) fn degrees(self) -> f32 {
        match self {
            PageRotate::Rotate0 => 0.0,
            PageRotate::Rotate90 => 90.0,
            PageRotate::Rotate180 => 180.0,
            PageRotate::Rotate270 => 270.0,
        }
    }

    /// Width and height of the rendered page: the visible box's extents,
    /// swapped by a quarter turn.
    pub(crate) fn display_size(self, sheet: &PageBox) -> (f32, f32) {
        let (width, height) = (sheet.width(), sheet.height());
        match self {
            PageRotate::Rotate0 | PageRotate::Rotate180 => (width, height),
            PageRotate::Rotate90 | PageRotate::Rotate270 => (height, width),
        }
    }

    /// Turn a sheet-frame box (lower-left origin, `y` up) into the display
    /// frame. Both edges are normalised first, so the result always has
    /// non-negative extents.
    pub(crate) fn sheet_box_to_display(
        self,
        sheet: &PageBox,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> (f32, f32, f32, f32) {
        let (x0, x1) = (x.min(x + width), x.max(x + width));
        let (y0, y1) = (y.min(y + height), y.max(y + height));
        let (w, h) = (x1 - x0, y1 - y0);
        let (sheet_w, sheet_h) = (sheet.width(), sheet.height());
        match self {
            PageRotate::Rotate0 => (x0, y0, w, h),
            PageRotate::Rotate90 => (y0, sheet_w - x1, h, w),
            PageRotate::Rotate180 => (sheet_w - x1, sheet_h - y1, w, h),
            PageRotate::Rotate270 => (sheet_h - y1, x0, h, w),
        }
    }

    /// Turn a region rect given in the display frame's top-left space
    /// (`[x1, y1, x2, y2]`, `y` down, on the rendered page) into the sheet
    /// frame's top-left space, where the region readers work.
    pub(crate) fn display_rect_to_sheet(self, sheet: &PageBox, rect: [f32; 4]) -> [f32; 4] {
        let [ax, ay, bx, by] = rect;
        let (x1, x2) = (ax.min(bx), ax.max(bx));
        let (y1, y2) = (ay.min(by), ay.max(by));
        let (display_w, display_h) = self.display_size(sheet);
        match self {
            PageRotate::Rotate0 => [x1, y1, x2, y2],
            PageRotate::Rotate90 => [y1, display_w - x2, y2, display_w - x1],
            PageRotate::Rotate180 => [
                display_w - x2,
                display_h - y2,
                display_w - x1,
                display_h - y1,
            ],
            PageRotate::Rotate270 => [display_h - y2, x1, display_h - y1, x2],
        }
    }
}

/// The page's inheritable `/Rotate`, walking `/Parent` links like the page
/// boxes. The first ancestor carrying the key decides, as in renderers; a
/// missing or malformed value reads as `0`.
pub(crate) fn page_rotate(doc: &Document, page_id: ObjectId) -> PageRotate {
    let mut id = page_id;
    for _ in 0..32 {
        let Ok(dict) = doc.get_dictionary(id) else {
            return PageRotate::Rotate0;
        };
        if let Ok(value) = dict.get(b"Rotate") {
            let value = match value {
                Object::Reference(reference) => match doc.get_object(*reference) {
                    Ok(object) => object,
                    Err(_) => return PageRotate::Rotate0,
                },
                direct => direct,
            };
            return super::get_number(value)
                .map(|degrees| PageRotate::from_degrees(degrees as f64))
                .unwrap_or_default();
        }
        match dict.get(b"Parent") {
            Ok(Object::Reference(parent)) => id = *parent,
            _ => return PageRotate::Rotate0,
        }
    }
    PageRotate::Rotate0
}

/// How one extracted page maps onto its rendered image: the page's `/Rotate`
/// and the visible box it turns.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DisplayPage {
    pub(crate) rotate: PageRotate,
    pub(crate) sheet: PageBox,
}

impl DisplayPage {
    /// The display mapping of `page_id`, whose geometry was shifted into
    /// `sheet` (the visible page box the extractor used).
    pub(crate) fn new(doc: &Document, page_id: ObjectId, sheet: PageBox) -> DisplayPage {
        DisplayPage {
            rotate: page_rotate(doc, page_id),
            sheet,
        }
    }

    /// A region rect given on the rendered page (top-left space), expressed
    /// in the sheet frame's top-left space.
    pub(crate) fn region_to_sheet(&self, rect: [f32; 4]) -> [f32; 4] {
        self.rotate.display_rect_to_sheet(&self.sheet, rect)
    }
}

/// Move one page's items from the frame the extractor reports them in — the
/// sheet frame, turned by `turn` when the page's text was predominantly
/// rotated — into the display frame of a page rendered with `rotate`.
/// Baseline angles follow: the dominant runs of a turned page read as `0`
/// in the turned frame and, once the page is rendered the way it reads, as
/// `0` in the display frame too. A page that is neither turned nor rotated
/// is left untouched.
pub(crate) fn items_to_display_frame(
    items: &mut [TextItem],
    turn: PageRotation,
    rotate: PageRotate,
    sheet: &PageBox,
) {
    if turn == PageRotation::Upright && rotate == PageRotate::Rotate0 {
        return;
    }
    for item in items {
        turn.unrotate_box(&mut item.x, &mut item.y, &mut item.width, &mut item.height);
        let (x, y, width, height) =
            rotate.sheet_box_to_display(sheet, item.x, item.y, item.width, item.height);
        item.x = x;
        item.y = y;
        item.width = width;
        item.height = height;
        // Only text runs carry a baseline angle; placeholders keep the `0`
        // they were extracted with (see `correct_rotated_page`).
        if matches!(item.item_type, ItemType::Text) {
            item.rotation = normalize_degrees(
                item.rotation - turn.baseline_rebase_degrees() - rotate.degrees(),
            );
        }
    }
}

/// [`items_to_display_frame`] for a whole document's extraction:
/// `page_rotations` holds the turn of every predominantly rotated page
/// (pages absent from it are upright), as the position pipeline reports it,
/// and each page's `/Rotate` and visible box are read from `doc`.
pub(crate) fn document_items_to_display_frame(
    doc: &Document,
    items: &mut [TextItem],
    page_rotations: &HashMap<u32, PageRotation>,
) {
    let pages = doc.get_pages();
    let mut frames: HashMap<u32, (PageRotation, PageRotate, PageBox)> = HashMap::new();
    for item in items {
        let (turn, rotate, sheet) = *frames.entry(item.page).or_insert_with(|| {
            let turn = page_rotations
                .get(&item.page)
                .copied()
                .unwrap_or(PageRotation::Upright);
            // The same fallback the pipeline shifts into when a page has no
            // usable box, so the display frame agrees with the sheet frame.
            let (rotate, sheet) = match pages.get(&item.page) {
                Some(&page_id) => (
                    page_rotate(doc, page_id),
                    visible_page_box(doc, page_id).unwrap_or(PageBox::LETTER),
                ),
                None => (PageRotate::Rotate0, PageBox::LETTER),
            };
            (turn, rotate, sheet)
        });
        items_to_display_frame(std::slice::from_mut(item), turn, rotate, &sheet);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    fn item(x: f32, y: f32, width: f32, height: f32, rotation: f32) -> TextItem {
        TextItem {
            text: "a".into(),
            x,
            y,
            width,
            height,
            font: String::new(),
            font_tag: String::new(),
            legacy_symbol_rewrite: false,
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            font_weight: None,
            bold_source: None,
            fixed_pitch: None,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
            baseline_shift: 0.0,
            rotation,
            advance_known: true,
        }
    }

    fn geometry(item: &TextItem) -> (f32, f32, f32, f32, f32) {
        (item.x, item.y, item.width, item.height, item.rotation)
    }

    /// One-page document with an optional `/Rotate` on the page and on the
    /// `/Pages` node.
    fn doc_with_rotate(page: Option<Object>, parent: Option<Object>) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), 612.into(), 792.into()]),
        };
        if let Some(rotate) = page {
            page_dict.set("Rotate", rotate);
        }
        let page_id = doc.add_object(page_dict);
        let mut pages = dictionary! {
            "Type" => "Pages",
            "Count" => 1,
            "Kids" => vec![Object::Reference(page_id)],
        };
        if let Some(rotate) = parent {
            pages.set("Rotate", rotate);
        }
        doc.objects.insert(pages_id, pages.into());
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        (doc, page_id)
    }

    #[test]
    fn rotate_snaps_to_a_right_angle_in_a_full_turn() {
        use PageRotate::*;
        for (degrees, expected) in [
            (0.0, Rotate0),
            (90.0, Rotate90),
            (180.0, Rotate180),
            (270.0, Rotate270),
            (360.0, Rotate0),
            (-90.0, Rotate270),
            (-180.0, Rotate180),
            (-270.0, Rotate90),
            (450.0, Rotate90),
            (-450.0, Rotate270),
            (720.0, Rotate0),
            (810.0, Rotate90),
            (89.6, Rotate90),
            (44.0, Rotate0),
            (45.0, Rotate90),
            (134.9, Rotate90),
            (135.0, Rotate180),
            (f64::NAN, Rotate0),
            (f64::INFINITY, Rotate0),
            (f64::NEG_INFINITY, Rotate0),
        ] {
            assert_eq!(PageRotate::from_degrees(degrees), expected, "{degrees}");
        }
    }

    #[test]
    fn rotate_is_read_from_the_page_and_normalised() {
        for (value, expected) in [
            (Object::Integer(90), PageRotate::Rotate90),
            (Object::Real(270.0), PageRotate::Rotate270),
            (Object::Integer(-90), PageRotate::Rotate270),
            (Object::Integer(450), PageRotate::Rotate90),
            (Object::Integer(180), PageRotate::Rotate180),
        ] {
            let (doc, page_id) = doc_with_rotate(Some(value.clone()), None);
            assert_eq!(page_rotate(&doc, page_id), expected, "{value:?}");
        }
    }

    #[test]
    fn rotate_is_inherited_from_the_page_tree() {
        let (doc, page_id) = doc_with_rotate(None, Some(Object::Integer(90)));
        assert_eq!(page_rotate(&doc, page_id), PageRotate::Rotate90);
        // The page's own value wins over the inherited one.
        let (doc, page_id) = doc_with_rotate(Some(Object::Integer(180)), Some(Object::Integer(90)));
        assert_eq!(page_rotate(&doc, page_id), PageRotate::Rotate180);
    }

    #[test]
    fn missing_or_malformed_rotate_reads_as_zero() {
        let (doc, page_id) = doc_with_rotate(None, None);
        assert_eq!(page_rotate(&doc, page_id), PageRotate::Rotate0);
        let (doc, page_id) = doc_with_rotate(Some(Object::Name(b"ninety".to_vec())), None);
        assert_eq!(page_rotate(&doc, page_id), PageRotate::Rotate0);
        // The first ancestor carrying the key decides, malformed or not.
        let (doc, page_id) = doc_with_rotate(
            Some(Object::Name(b"ninety".to_vec())),
            Some(Object::Integer(90)),
        );
        assert_eq!(page_rotate(&doc, page_id), PageRotate::Rotate0);
    }

    #[test]
    fn indirect_rotate_is_resolved() {
        let (mut doc, page_id) = doc_with_rotate(None, None);
        let rotate_id = doc.add_object(Object::Integer(270));
        doc.get_dictionary_mut(page_id)
            .unwrap()
            .set("Rotate", Object::Reference(rotate_id));
        assert_eq!(page_rotate(&doc, page_id), PageRotate::Rotate270);
    }

    #[test]
    fn sheet_boxes_land_where_a_renderer_draws_them() {
        let sheet = PageBox::LETTER;
        let (x, y, w, h) = (100.0, 200.0, 50.0, 10.0);
        assert_eq!(
            PageRotate::Rotate0.sheet_box_to_display(&sheet, x, y, w, h),
            (100.0, 200.0, 50.0, 10.0)
        );
        assert_eq!(
            PageRotate::Rotate90.sheet_box_to_display(&sheet, x, y, w, h),
            (200.0, 462.0, 10.0, 50.0)
        );
        assert_eq!(
            PageRotate::Rotate180.sheet_box_to_display(&sheet, x, y, w, h),
            (462.0, 582.0, 50.0, 10.0)
        );
        assert_eq!(
            PageRotate::Rotate270.sheet_box_to_display(&sheet, x, y, w, h),
            (582.0, 100.0, 10.0, 50.0)
        );
        // Negative extents describe the same box.
        assert_eq!(
            PageRotate::Rotate90.sheet_box_to_display(&sheet, 150.0, 210.0, -50.0, -10.0),
            (200.0, 462.0, 10.0, 50.0)
        );
        assert_eq!(PageRotate::Rotate0.display_size(&sheet), (612.0, 792.0));
        assert_eq!(PageRotate::Rotate90.display_size(&sheet), (792.0, 612.0));
        assert_eq!(PageRotate::Rotate180.display_size(&sheet), (612.0, 792.0));
        assert_eq!(PageRotate::Rotate270.display_size(&sheet), (792.0, 612.0));
    }

    /// The top-left rect of a y-up box on a page `height` tall.
    fn top_left_rect(x: f32, y: f32, w: f32, h: f32, height: f32) -> [f32; 4] {
        [x, height - y - h, x + w, height - y]
    }

    #[test]
    fn display_rects_come_back_to_the_sheet_rect_of_the_same_box() {
        for sheet in [
            PageBox::LETTER,
            PageBox::from_corners(50.0, 60.0, 350.0, 460.0).unwrap(),
        ] {
            let (x, y, w, h) = (20.0, 30.0, 40.0, 12.0);
            let expected = top_left_rect(x, y, w, h, sheet.height());
            for rotate in [
                PageRotate::Rotate0,
                PageRotate::Rotate90,
                PageRotate::Rotate180,
                PageRotate::Rotate270,
            ] {
                let (dx, dy, dw, dh) = rotate.sheet_box_to_display(&sheet, x, y, w, h);
                let (_, display_h) = rotate.display_size(&sheet);
                let display_rect = top_left_rect(dx, dy, dw, dh, display_h);
                assert_eq!(
                    rotate.display_rect_to_sheet(&sheet, display_rect),
                    expected,
                    "{rotate:?} on {sheet:?}"
                );
                // Corner order does not matter.
                let [a, b, c, d] = display_rect;
                assert_eq!(rotate.display_rect_to_sheet(&sheet, [c, d, a, b]), expected);
            }
        }
    }

    #[test]
    fn display_rects_follow_the_documented_formulas() {
        // Display page 792 x 612 for a quarter turn of a Letter sheet.
        let sheet = PageBox::LETTER;
        let rect = [200.0, 100.0, 210.0, 150.0];
        assert_eq!(
            PageRotate::Rotate90.display_rect_to_sheet(&sheet, rect),
            [100.0, 792.0 - 210.0, 150.0, 792.0 - 200.0]
        );
        assert_eq!(
            PageRotate::Rotate180.display_rect_to_sheet(&sheet, rect),
            [612.0 - 210.0, 792.0 - 150.0, 612.0 - 200.0, 792.0 - 100.0]
        );
        assert_eq!(
            PageRotate::Rotate270.display_rect_to_sheet(&sheet, rect),
            [612.0 - 150.0, 200.0, 612.0 - 100.0, 210.0]
        );
        assert_eq!(
            PageRotate::Rotate0.display_rect_to_sheet(&sheet, rect),
            rect
        );
    }

    #[test]
    fn offset_crop_box_uses_the_visible_extents_only() {
        // A 300 x 400 visible box whose origin is off (0, 0): the display
        // frame depends on its extents, never on where it sits on the sheet.
        let sheet = PageBox::from_corners(50.0, 60.0, 350.0, 460.0).unwrap();
        assert_eq!(
            PageRotate::Rotate90.sheet_box_to_display(&sheet, 20.0, 30.0, 40.0, 12.0),
            (30.0, 240.0, 12.0, 40.0)
        );
        assert_eq!(PageRotate::Rotate90.display_size(&sheet), (400.0, 300.0));
        assert_eq!(
            PageRotate::Rotate180.sheet_box_to_display(&sheet, 20.0, 30.0, 40.0, 12.0),
            (240.0, 358.0, 40.0, 12.0)
        );
    }

    #[test]
    fn turned_pages_are_unturned_before_the_display_turn() {
        let sheet = PageBox::LETTER;
        // A bottom-to-top run at `Tm [0 1 -1 0 40 420]`, 12pt, 200pt long:
        // sheet box x ∈ [28, 40], y ∈ [420, 620], baseline angle 90. The
        // extractor reports it in the counter-clockwise-turned frame.
        let (mut x, mut y, mut w, mut h) = (28.0, 420.0, 12.0, 200.0);
        PageRotation::Ccw.rotate_box(&mut x, &mut y, &mut w, &mut h);
        assert_eq!((x, y, w, h), (420.0, -40.0, 200.0, 12.0));
        let mut items = vec![item(x, y, w, h, 0.0)];
        // Rendered under /Rotate 90 it is a horizontal line 420pt from the
        // display page's left edge whose top sits 28pt (40 - h) below the
        // top edge of the 792 x 612 display page.
        items_to_display_frame(&mut items, PageRotation::Ccw, PageRotate::Rotate90, &sheet);
        assert_eq!(geometry(&items[0]), (420.0, 572.0, 200.0, 12.0, 0.0));
        assert_eq!(
            PageRotate::Rotate90.sheet_box_to_display(&sheet, 28.0, 420.0, 12.0, 200.0),
            (420.0, 572.0, 200.0, 12.0)
        );

        // An upright stray on that page (sheet box (72, 700, 100, 11),
        // angle 0) reads as 270 in the turned frame and renders top-to-bottom.
        let (mut x, mut y, mut w, mut h) = (72.0, 700.0, 100.0, 11.0);
        PageRotation::Ccw.rotate_box(&mut x, &mut y, &mut w, &mut h);
        let mut items = vec![item(x, y, w, h, 270.0)];
        items_to_display_frame(&mut items, PageRotation::Ccw, PageRotate::Rotate90, &sheet);
        assert_eq!(geometry(&items[0]), (700.0, 440.0, 11.0, 100.0, 270.0));

        // A top-to-bottom run at `Tm [0 -1 1 0 300 700]`, 12pt, 200pt long:
        // sheet box x ∈ [300, 312], y ∈ [500, 700], angle 270, reported in
        // the clockwise-turned frame. /Rotate 270 renders it horizontally.
        let (mut x, mut y, mut w, mut h) = (300.0, 500.0, 12.0, 200.0);
        PageRotation::Cw.rotate_box(&mut x, &mut y, &mut w, &mut h);
        assert_eq!((x, y, w, h), (-700.0, 300.0, 200.0, 12.0));
        let mut items = vec![item(x, y, w, h, 0.0)];
        items_to_display_frame(&mut items, PageRotation::Cw, PageRotate::Rotate270, &sheet);
        assert_eq!(geometry(&items[0]), (92.0, 300.0, 200.0, 12.0, 0.0));

        // Undoing the turn alone (an unrotated page) restores the sheet box.
        let mut items = vec![item(x, y, w, h, 0.0)];
        items_to_display_frame(&mut items, PageRotation::Cw, PageRotate::Rotate0, &sheet);
        assert_eq!(geometry(&items[0]), (300.0, 500.0, 12.0, 200.0, 270.0));

        // Placeholders turn with the page but never gain a baseline angle.
        let mut placeholder = item(-700.0, 300.0, 200.0, 12.0, 0.0);
        placeholder.item_type = ItemType::Image;
        let mut items = vec![placeholder];
        items_to_display_frame(&mut items, PageRotation::Cw, PageRotate::Rotate270, &sheet);
        assert_eq!(geometry(&items[0]), (92.0, 300.0, 200.0, 12.0, 0.0));
    }

    #[test]
    fn every_turn_and_rotate_pair_unturns_then_turns_with_the_page() {
        let sheet = PageBox::LETTER;
        // Sheet boxes with their baseline angle, each on a page turned the
        // way its text votes: upright text on an unturned page, a
        // bottom-to-top run on a counter-clockwise-turned one, a
        // top-to-bottom run on a clockwise-turned one, and an upright stray
        // on each turned page.
        let cases = [
            (PageRotation::Upright, (72.0, 700.0, 100.0, 11.0), 0.0),
            (PageRotation::Ccw, (28.0, 420.0, 12.0, 200.0), 90.0),
            (PageRotation::Ccw, (72.0, 700.0, 100.0, 11.0), 0.0),
            (PageRotation::Cw, (300.0, 500.0, 12.0, 200.0), 270.0),
            (PageRotation::Cw, (72.0, 700.0, 100.0, 11.0), 0.0),
        ];
        let rotates = [
            PageRotate::Rotate0,
            PageRotate::Rotate90,
            PageRotate::Rotate180,
            PageRotate::Rotate270,
        ];
        for (turn, (x, y, w, h), sheet_rotation) in cases {
            for rotate in rotates {
                // The item as the extractor reports it: box and baseline
                // angle turned with the page.
                let (mut tx, mut ty, mut tw, mut th) = (x, y, w, h);
                turn.rotate_box(&mut tx, &mut ty, &mut tw, &mut th);
                let reported = normalize_degrees(sheet_rotation + turn.baseline_rebase_degrees());
                let mut items = vec![item(tx, ty, tw, th, reported)];
                items_to_display_frame(&mut items, turn, rotate, &sheet);
                // Undoing the turn gives the sheet box back, and the display
                // turn is then the plain sheet-to-display mapping.
                let (dx, dy, dw, dh) = rotate.sheet_box_to_display(&sheet, x, y, w, h);
                let display_rotation = normalize_degrees(sheet_rotation - rotate.degrees());
                assert_eq!(
                    geometry(&items[0]),
                    (dx, dy, dw, dh, display_rotation),
                    "{turn:?} under {rotate:?}"
                );
            }
        }
    }

    #[test]
    fn upright_text_turns_with_the_page() {
        let sheet = PageBox::LETTER;
        for (rotate, expected) in [
            (PageRotate::Rotate90, (700.0, 440.0, 11.0, 100.0, 270.0)),
            (PageRotate::Rotate180, (440.0, 81.0, 100.0, 11.0, 180.0)),
            (PageRotate::Rotate270, (81.0, 72.0, 11.0, 100.0, 90.0)),
        ] {
            let mut items = vec![item(72.0, 700.0, 100.0, 11.0, 0.0)];
            items_to_display_frame(&mut items, PageRotation::Upright, rotate, &sheet);
            assert_eq!(geometry(&items[0]), expected, "{rotate:?}");
        }
    }

    #[test]
    fn untouched_when_neither_turned_nor_rotated() {
        // Not even extents are normalised: the display frame of an unrotated
        // upright page is the sheet frame, byte for byte.
        let mut items = vec![item(150.0, 210.0, -50.0, -10.0, 0.0)];
        items_to_display_frame(
            &mut items,
            PageRotation::Upright,
            PageRotate::Rotate0,
            &PageBox::LETTER,
        );
        assert_eq!(geometry(&items[0]), (150.0, 210.0, -50.0, -10.0, 0.0));
    }

    #[test]
    fn document_items_follow_their_own_page() {
        // Page 1 carries /Rotate 90, page 2 nothing; the visible box of
        // page 1 is an offset CropBox so its extents, not the MediaBox's,
        // must drive the turn.
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page1 = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), 400.into(), 500.into()]),
            "CropBox" => Object::Array(vec![50.into(), 60.into(), 350.into(), 460.into()]),
            "Rotate" => Object::Integer(90),
        });
        let page2 = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), 612.into(), 792.into()]),
        });
        doc.objects.insert(
            pages_id,
            dictionary! {
                "Type" => "Pages",
                "Count" => 2,
                "Kids" => vec![Object::Reference(page1), Object::Reference(page2)],
            }
            .into(),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut first = item(20.0, 30.0, 40.0, 12.0, 0.0);
        first.page = 1;
        let mut second = item(20.0, 30.0, 40.0, 12.0, 0.0);
        second.page = 2;
        let mut items = vec![first, second];
        document_items_to_display_frame(&doc, &mut items, &HashMap::new());
        assert_eq!(geometry(&items[0]), (30.0, 240.0, 12.0, 40.0, 270.0));
        assert_eq!(geometry(&items[1]), (20.0, 30.0, 40.0, 12.0, 0.0));

        // A turned page 2 is unturned even though it is not rotated.
        let mut turned = item(30.0, -60.0, 12.0, 40.0, 0.0);
        turned.page = 2;
        let mut items = vec![turned];
        let rotations = HashMap::from([(2, PageRotation::Ccw)]);
        document_items_to_display_frame(&doc, &mut items, &rotations);
        assert_eq!(geometry(&items[0]), (20.0, 30.0, 40.0, 12.0, 90.0));
    }
}
