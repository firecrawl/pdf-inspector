//! Visible page box resolution and the coordinate frame shared by every
//! position-returning and region-consuming API.
//!
//! Renderers (MuPDF, PDFium, pdf.js) draw `CropBox ∩ MediaBox`, so a layout
//! model that runs on a page image reports coordinates relative to that box,
//! not to raw PDF user space. Positioned items are shifted into the same
//! frame before they leave the crate, and region inputs are interpreted in
//! it, so callers can intersect the two without knowing about the CropBox.
//! The MediaBox origin itself is often non-zero, so even pages without a
//! CropBox can need the shift.

use lopdf::{Document, Object, ObjectId};
use std::collections::HashSet;

use super::geometry::PageRotation;
use crate::types::{PdfLine, PdfRect, TextItem};

/// The visible page box in raw PDF user space, normalized so `x0 < x1` and
/// `y0 < y1`. `/Rotate` is not applied. It is the box every position the
/// crate reports is relative to (see [`page_boxes_mem`](crate::page_boxes_mem)).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl PageBox {
    /// US Letter — what renderers assume when a page carries no usable box.
    pub const LETTER: PageBox = PageBox {
        x0: 0.0,
        y0: 0.0,
        x1: 612.0,
        y1: 792.0,
    };

    /// Normalize two corners into a box. `None` when a coordinate is not
    /// finite or the box has no area.
    pub fn from_corners(ax: f32, ay: f32, bx: f32, by: f32) -> Option<PageBox> {
        if ![ax, ay, bx, by].iter().all(|v| v.is_finite()) {
            return None;
        }
        let page_box = PageBox {
            x0: ax.min(bx),
            y0: ay.min(by),
            x1: ax.max(bx),
            y1: ay.max(by),
        };
        (page_box.x1 > page_box.x0 && page_box.y1 > page_box.y0).then_some(page_box)
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    /// `None` when the boxes do not overlap. Built directly rather than via
    /// [`PageBox::from_corners`], whose corner normalization would turn a
    /// disjoint pair into a phantom box.
    fn intersect(&self, other: &PageBox) -> Option<PageBox> {
        let page_box = PageBox {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (page_box.x1 > page_box.x0 && page_box.y1 > page_box.y0).then_some(page_box)
    }

    /// Offset that moves raw user-space geometry into the visible-box frame,
    /// where the box's lower-left corner is the origin.
    ///
    /// A page whose frame was turned by `content_stream::correct_rotated_page`
    /// (see [`PageRotation`]) had its points mapped `(x, y) → (y, -x)` or
    /// `(x, y) → (-y, x)`. Translating before that turn is the same as
    /// applying the turned offset afterwards — `(-y0, +x0)` for a
    /// counter-clockwise turn, `(+y0, -x0)` for a clockwise one — so the
    /// items, rects and lines it produced stay consistent with region bounds
    /// computed from the visible box height.
    fn shift(&self, rotation: PageRotation) -> (f32, f32) {
        rotation.rotate_point(-self.x0, -self.y0)
    }

    /// Shift one page's items, rects and lines into the visible-box frame.
    pub(crate) fn translate_page(
        &self,
        items: &mut [TextItem],
        rects: &mut [PdfRect],
        lines: &mut [PdfLine],
        rotation: PageRotation,
    ) {
        let (dx, dy) = self.shift(rotation);
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        for item in items {
            item.x += dx;
            item.y += dy;
        }
        for rect in rects {
            rect.x += dx;
            rect.y += dy;
        }
        for line in lines {
            line.x1 += dx;
            line.y1 += dy;
            line.x2 += dx;
            line.y2 += dy;
        }
    }

    /// Shift items alone into the visible-box frame.
    pub(crate) fn translate_items(&self, items: &mut [TextItem], rotation: PageRotation) {
        self.translate_page(items, &mut [], &mut [], rotation);
    }
}

/// Resolve the visible page box the way renderers do (PDFium's
/// `CPDF_Page::UpdateDimensions`): the page's `/CropBox` cut to its
/// `/MediaBox`, where a missing or empty `/CropBox` leaves the `/MediaBox`
/// and a missing or empty `/MediaBox` is US Letter. Both attributes are
/// inheritable, so page-tree ancestors are consulted, and the first
/// dictionary that carries the key decides: a page-level entry that is
/// malformed or empty is *not* replaced by an ancestor's valid one (PDFium
/// takes the page's own key and then applies its empty fallback). An entry
/// is a rectangle only when it is an array of exactly four operands; an
/// operand that is not a number reads as zero. A CropBox that leaves an
/// empty cut with the MediaBox keeps the MediaBox.
///
/// `None` when neither box yields a usable rectangle, that is, when a
/// renderer would fall back to [`PageBox::LETTER`] on its own; callers that
/// need a concrete frame use that default.
pub fn visible_page_box(doc: &Document, page_id: ObjectId) -> Option<PageBox> {
    let media_box = find_inherited_box(doc, page_id, b"MediaBox").flatten();
    let crop_box = find_inherited_box(doc, page_id, b"CropBox").flatten();
    match (media_box, crop_box) {
        (Some(media), Some(crop)) => Some(media.intersect(&crop).unwrap_or(media)),
        (Some(media), None) => Some(media),
        (None, Some(crop)) => Some(PageBox::LETTER.intersect(&crop).unwrap_or(PageBox::LETTER)),
        (None, None) => None,
    }
}

/// Read an inheritable rectangle attribute, walking `/Parent` links. The
/// outer `None` means no dictionary of the chain carries the key; the first
/// one that does decides, and `Some(None)` is a present entry that is not a
/// usable rectangle (a direct `null`, wrong operand count, no area, not
/// finite). An indirect entry is resolved first, and a reference to a missing
/// object counts as absent, so the walk goes on to the parent, as PDFium and
/// ISO 32000-1 7.3.10 have it. The chain is followed until it ends or repeats,
/// with no depth limit, like PDFium.
fn find_inherited_box(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<Option<PageBox>> {
    let mut id = page_id;
    let mut visited = HashSet::new();
    while visited.insert(id) {
        let dict = doc.get_dictionary(id).ok()?;
        if let Ok(obj) = dict.get(key) {
            let resolved = match obj {
                Object::Reference(reference) => doc.get_object(*reference).ok(),
                direct => Some(direct),
            };
            if let Some(resolved) = resolved {
                return Some(box_from_array(doc, resolved));
            }
        }
        match dict.get(b"Parent") {
            Ok(Object::Reference(parent)) => id = *parent,
            _ => return None,
        }
    }
    None
}

/// The rectangle an already resolved entry denotes, as a renderer reads it:
/// an array of exactly four operands, indirect operands resolved, a
/// non-number as zero, corners normalized.
fn box_from_array(doc: &Document, obj: &Object) -> Option<PageBox> {
    let Object::Array(array) = obj else {
        return None;
    };
    if array.len() != 4 {
        return None;
    }
    let number = |value: &Object| -> f32 {
        match value {
            Object::Reference(reference) => doc
                .get_object(*reference)
                .ok()
                .and_then(super::get_number)
                .unwrap_or(0.0),
            _ => super::get_number(value).unwrap_or(0.0),
        }
    };
    PageBox::from_corners(
        number(&array[0]),
        number(&array[1]),
        number(&array[2]),
        number(&array[3]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;
    use lopdf::dictionary;

    fn boxed(values: [i64; 4]) -> Object {
        Object::Array(values.iter().map(|&v| v.into()).collect())
    }

    /// One-page document; `page_box_keys` go on the page, `parent_keys` on
    /// the `/Pages` node.
    fn doc_with_boxes(
        page_keys: &[(&str, [i64; 4])],
        parent_keys: &[(&str, [i64; 4])],
    ) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
        };
        for (key, values) in page_keys {
            page.set(*key, boxed(*values));
        }
        let page_id = doc.add_object(page);
        let mut pages = dictionary! {
            "Type" => "Pages",
            "Count" => 1,
            "Kids" => vec![Object::Reference(page_id)],
        };
        for (key, values) in parent_keys {
            pages.set(*key, boxed(*values));
        }
        doc.objects.insert(pages_id, pages.into());
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        (doc, page_id)
    }

    fn page_box(x0: f32, y0: f32, x1: f32, y1: f32) -> PageBox {
        PageBox { x0, y0, x1, y1 }
    }

    #[test]
    fn media_box_alone_is_the_visible_box() {
        let (doc, page_id) = doc_with_boxes(&[("MediaBox", [0, 0, 612, 792])], &[]);
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(0.0, 0.0, 612.0, 792.0))
        );
    }

    #[test]
    fn crop_box_inside_media_box_wins() {
        let (doc, page_id) = doc_with_boxes(
            &[
                ("MediaBox", [0, 0, 400, 500]),
                ("CropBox", [50, 60, 350, 460]),
            ],
            &[],
        );
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(50.0, 60.0, 350.0, 460.0))
        );
    }

    #[test]
    fn crop_box_is_intersected_with_an_offset_media_box() {
        // The observed shape: MediaBox origin off (0,0) and a CropBox that
        // pokes below it. Renderers show the intersection.
        let (doc, page_id) = doc_with_boxes(
            &[
                ("MediaBox", [36, 36, 648, 819]),
                ("CropBox", [36, 0, 648, 783]),
            ],
            &[],
        );
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(36.0, 36.0, 648.0, 783.0))
        );
    }

    #[test]
    fn disjoint_crop_box_falls_back_to_media_box() {
        let (doc, page_id) = doc_with_boxes(
            &[
                ("MediaBox", [0, 0, 400, 500]),
                ("CropBox", [900, 900, 950, 950]),
            ],
            &[],
        );
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(0.0, 0.0, 400.0, 500.0))
        );
    }

    #[test]
    fn reversed_corners_are_normalized() {
        let (doc, page_id) = doc_with_boxes(&[("MediaBox", [612, 792, 0, 0])], &[]);
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(0.0, 0.0, 612.0, 792.0))
        );
    }

    #[test]
    fn boxes_are_inherited_from_the_page_tree() {
        let (doc, page_id) = doc_with_boxes(
            &[],
            &[
                ("MediaBox", [0, 0, 400, 500]),
                ("CropBox", [50, 60, 350, 460]),
            ],
        );
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(50.0, 60.0, 350.0, 460.0))
        );
        // A page-level MediaBox overrides the inherited one; the CropBox is
        // still inherited.
        let (doc, page_id) = doc_with_boxes(
            &[("MediaBox", [0, 0, 300, 300])],
            &[
                ("MediaBox", [0, 0, 400, 500]),
                ("CropBox", [50, 60, 350, 460]),
            ],
        );
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(50.0, 60.0, 300.0, 300.0))
        );
    }

    #[test]
    fn indirect_box_operands_are_resolved() {
        // `/CropBox [50 60 350 7 0 R]` with `7 0 obj 460 endobj`: dropping the
        // reference would leave three numbers, skip the CropBox, and resolve
        // the page against the MediaBox instead.
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let top = doc.add_object(Object::Real(460.0));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => boxed([0, 0, 400, 500]),
            "CropBox" => Object::Array(vec![
                50.into(),
                60.into(),
                350.into(),
                Object::Reference(top),
            ]),
        });
        doc.objects.insert(
            pages_id,
            dictionary! {
                "Type" => "Pages",
                "Count" => 1,
                "Kids" => vec![Object::Reference(page_id)],
            }
            .into(),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(50.0, 60.0, 350.0, 460.0))
        );
    }

    #[test]
    fn crop_box_without_media_box_is_measured_against_letter() {
        let (doc, page_id) = doc_with_boxes(&[("CropBox", [100, 100, 700, 900])], &[]);
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(100.0, 100.0, 612.0, 792.0))
        );
    }

    #[test]
    fn a_broken_page_box_is_not_replaced_by_an_inherited_valid_one() {
        // PDFium takes the page's own key and applies its empty fallback:
        // an empty page CropBox leaves the MediaBox, even though the page
        // tree carries a valid CropBox.
        let (doc, page_id) = doc_with_boxes(
            &[("MediaBox", [0, 0, 400, 500]), ("CropBox", [0, 0, 0, 0])],
            &[("CropBox", [10, 10, 300, 400])],
        );
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(0.0, 0.0, 400.0, 500.0))
        );
        // The same for a three-number CropBox and a four-number MediaBox
        // with no area over an inherited valid MediaBox.
        let mut doc3 = doc_with_boxes(
            &[("MediaBox", [0, 0, 400, 500])],
            &[("CropBox", [10, 10, 300, 400])],
        );
        let page = doc3.0.get_dictionary_mut(doc3.1).unwrap();
        page.set(
            "CropBox",
            Object::Array(vec![0.into(), 0.into(), 300.into()]),
        );
        assert_eq!(
            visible_page_box(&doc3.0, doc3.1),
            Some(page_box(0.0, 0.0, 400.0, 500.0))
        );
        let (doc, page_id) = doc_with_boxes(
            &[("MediaBox", [0, 0, 0, 842])],
            &[("MediaBox", [0, 0, 595, 842])],
        );
        assert_eq!(visible_page_box(&doc, page_id), None);
    }

    #[test]
    fn a_box_is_a_rectangle_only_with_exactly_four_operands() {
        let (mut doc, page_id) = doc_with_boxes(&[("MediaBox", [0, 0, 400, 500])], &[]);
        let page = doc.get_dictionary_mut(page_id).unwrap();
        page.set(
            "CropBox",
            Object::Array(vec![10.into(), 10.into(), 300.into(), 400.into(), 9.into()]),
        );
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(0.0, 0.0, 400.0, 500.0))
        );
        // A non-number operand reads as zero.
        let page = doc.get_dictionary_mut(page_id).unwrap();
        page.set(
            "CropBox",
            Object::Array(vec![
                10.into(),
                Object::string_literal("x"),
                300.into(),
                400.into(),
            ]),
        );
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(10.0, 0.0, 300.0, 400.0))
        );
    }

    #[test]
    fn page_boxes_mem_reports_the_visible_box_of_every_page_in_order() {
        // Broken page CropBox over an inherited valid one: the page keeps the
        // MediaBox, as a renderer sizes it.
        let (mut doc, _) = doc_with_boxes(
            &[("MediaBox", [0, 0, 400, 500]), ("CropBox", [0, 0, 0, 0])],
            &[("CropBox", [10, 10, 300, 400])],
        );
        let mut buffer = Vec::new();
        doc.save_to(&mut buffer).expect("serialize");
        let boxes = crate::page_boxes_mem(&buffer).expect("page boxes");
        assert_eq!(boxes, vec![page_box(0.0, 0.0, 400.0, 500.0)]);
        // No box anywhere: the frame the position APIs use.
        let (mut doc, _) = doc_with_boxes(&[], &[]);
        let mut buffer = Vec::new();
        doc.save_to(&mut buffer).expect("serialize");
        assert_eq!(
            crate::page_boxes_mem(&buffer).expect("page boxes"),
            vec![PageBox::LETTER]
        );
        // A MediaBox inherited from the page tree.
        let (mut doc, _) = doc_with_boxes(&[], &[("MediaBox", [0, 0, 595, 842])]);
        let mut buffer = Vec::new();
        doc.save_to(&mut buffer).expect("serialize");
        assert_eq!(
            crate::page_boxes_mem(&buffer).expect("page boxes"),
            vec![page_box(0.0, 0.0, 595.0, 842.0)]
        );
    }

    #[test]
    fn a_reference_to_a_missing_object_is_absent_and_the_walk_goes_on() {
        let (mut doc, page_id) = doc_with_boxes(
            &[("MediaBox", [0, 0, 400, 500])],
            &[("CropBox", [10, 10, 300, 400])],
        );
        let page = doc.get_dictionary_mut(page_id).unwrap();
        page.set("CropBox", Object::Reference((9999, 0)));
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(10.0, 10.0, 300.0, 400.0))
        );
        // The same for the MediaBox: the inherited one stands.
        let (mut doc, page_id) = doc_with_boxes(&[], &[("MediaBox", [0, 0, 595, 842])]);
        let page = doc.get_dictionary_mut(page_id).unwrap();
        page.set("MediaBox", Object::Reference((9999, 0)));
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(0.0, 0.0, 595.0, 842.0))
        );
    }

    #[test]
    fn a_null_entry_is_present_and_empty() {
        let (mut doc, page_id) = doc_with_boxes(
            &[("MediaBox", [0, 0, 400, 500])],
            &[("CropBox", [10, 10, 300, 400])],
        );
        let page = doc.get_dictionary_mut(page_id).unwrap();
        page.set("CropBox", Object::Null);
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(0.0, 0.0, 400.0, 500.0))
        );
        // An indirect entry that resolves to null is present too.
        let null_id = doc.add_object(Object::Null);
        let page = doc.get_dictionary_mut(page_id).unwrap();
        page.set("CropBox", Object::Reference(null_id));
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(0.0, 0.0, 400.0, 500.0))
        );
    }

    #[test]
    fn the_parent_chain_has_no_depth_limit_and_stops_at_a_repeat() {
        use lopdf::dictionary;
        // 300 nested /Pages nodes with the MediaBox only on the root.
        let mut doc = Document::with_version("1.5");
        let ids: Vec<ObjectId> = (0..301).map(|_| doc.new_object_id()).collect();
        for level in 0..300 {
            let mut node = dictionary! {
                "Type" => "Pages",
                "Count" => 1,
                "Kids" => vec![Object::Reference(ids[level + 1])],
            };
            if level > 0 {
                node.set("Parent", Object::Reference(ids[level - 1]));
            }
            if level == 0 {
                node.set("MediaBox", boxed([0, 0, 300, 300]));
            }
            doc.objects.insert(ids[level], node.into());
        }
        let page_id = ids[300];
        doc.objects.insert(
            page_id,
            dictionary! { "Type" => "Page", "Parent" => Object::Reference(ids[299]) }.into(),
        );
        assert_eq!(
            visible_page_box(&doc, page_id),
            Some(page_box(0.0, 0.0, 300.0, 300.0))
        );
        // The page tree is real: the document enumerates the page and the accessor reports it.
        let root = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(ids[0]),
        });
        doc.trailer.set("Root", Object::Reference(root));
        let mut buffer = Vec::new();
        doc.save_to(&mut buffer).expect("serialize");
        assert_eq!(
            crate::page_boxes_mem(&buffer).expect("page boxes"),
            vec![page_box(0.0, 0.0, 300.0, 300.0)]
        );
        // A /Parent loop ends the walk.
        let a = doc.new_object_id();
        let b = doc.add_object(dictionary! { "Parent" => Object::Reference(a) });
        doc.objects
            .insert(a, dictionary! { "Parent" => Object::Reference(b) }.into());
        assert_eq!(visible_page_box(&doc, a), None);
    }

    #[test]
    fn degenerate_boxes_are_ignored() {
        let (doc, page_id) = doc_with_boxes(&[("MediaBox", [0, 0, 0, 792])], &[]);
        assert_eq!(visible_page_box(&doc, page_id), None);
        let (doc, page_id) = doc_with_boxes(&[], &[]);
        assert_eq!(visible_page_box(&doc, page_id), None);
    }

    fn item(x: f32, y: f32) -> TextItem {
        TextItem {
            text: "a".into(),
            x,
            y,
            width: 10.0,
            height: 12.0,
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
            fill_color: None,
            stroke_color: None,
            render_mode: None,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
            baseline_shift: 0.0,
            rotation: 0.0,
            advance_known: true,
        }
    }

    #[test]
    fn translate_page_shifts_every_geometry_kind() {
        let page_box = page_box(50.0, 60.0, 350.0, 460.0);
        let mut items = vec![item(120.0, 300.0)];
        let mut rects = vec![PdfRect {
            x: 100.0,
            y: 200.0,
            width: 30.0,
            height: 5.0,
            page: 1,
        }];
        let mut lines = vec![PdfLine {
            x1: 60.0,
            y1: 70.0,
            x2: 160.0,
            y2: 70.0,
            page: 1,
        }];
        page_box.translate_page(&mut items, &mut rects, &mut lines, PageRotation::Upright);
        assert_eq!((items[0].x, items[0].y), (70.0, 240.0));
        assert_eq!((rects[0].x, rects[0].y), (50.0, 140.0));
        assert_eq!((rects[0].width, rects[0].height), (30.0, 5.0));
        assert_eq!(
            (lines[0].x1, lines[0].y1, lines[0].x2, lines[0].y2),
            (10.0, 10.0, 110.0, 10.0)
        );
    }

    #[test]
    fn rotated_translation_matches_translating_before_rotation() {
        // Shifting the raw point first and turning must equal turning first
        // and applying the turned shift, for both turns: counter-clockwise
        // maps (x, y) -> (y, -x), clockwise maps (x, y) -> (-y, x).
        let page_box = page_box(50.0, 60.0, 350.0, 460.0);
        let (raw_x, raw_y) = (120.0_f32, 300.0_f32);
        let (rel_x, rel_y) = (raw_x - page_box.x0, raw_y - page_box.y0);

        let mut items = vec![item(raw_y, -raw_x)];
        page_box.translate_items(&mut items, PageRotation::Ccw);
        assert_eq!((items[0].x, items[0].y), (rel_y, -rel_x));

        let mut items = vec![item(-raw_y, raw_x)];
        page_box.translate_items(&mut items, PageRotation::Cw);
        assert_eq!((items[0].x, items[0].y), (-rel_y, rel_x));
    }

    #[test]
    fn origin_box_is_a_no_op() {
        let mut items = vec![item(72.0, 700.0)];
        PageBox::LETTER.translate_items(&mut items, PageRotation::Upright);
        assert_eq!((items[0].x, items[0].y), (72.0, 700.0));
    }
}
