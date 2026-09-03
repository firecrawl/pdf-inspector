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

use super::geometry::PageRotation;
use crate::types::{PdfLine, PdfRect, TextItem};

/// The visible page box in raw PDF user space, normalized so `x0 < x1` and
/// `y0 < y1`. `/Rotate` is not applied.
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

/// Resolve the visible page box the way renderers do: `CropBox ∩ MediaBox`
/// when the page declares a CropBox that overlaps its MediaBox, otherwise the
/// MediaBox. Both attributes are inheritable, so page-tree ancestors are
/// consulted. A page with a CropBox but no MediaBox is measured against US
/// Letter, again like renderers. `None` when the page declares neither box
/// anywhere in its ancestry; callers that need a concrete frame fall back to
/// [`PageBox::LETTER`].
pub fn visible_page_box(doc: &Document, page_id: ObjectId) -> Option<PageBox> {
    let media_box = find_inherited_box(doc, page_id, b"MediaBox");
    let crop_box = find_inherited_box(doc, page_id, b"CropBox");
    match (media_box, crop_box) {
        (Some(media), Some(crop)) => Some(media.intersect(&crop).unwrap_or(media)),
        (Some(media), None) => Some(media),
        (None, Some(crop)) => Some(PageBox::LETTER.intersect(&crop).unwrap_or(PageBox::LETTER)),
        (None, None) => None,
    }
}

/// Read an inheritable rectangle attribute, walking `/Parent` links. The
/// first ancestor carrying a well-formed array wins, per the PDF spec.
fn find_inherited_box(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<PageBox> {
    let mut id = page_id;
    for _ in 0..32 {
        let dict = doc.get_dictionary(id).ok()?;
        if let Ok(obj) = dict.get(key) {
            let array = match obj {
                Object::Array(array) => Some(array),
                Object::Reference(reference) => match doc.get_object(*reference) {
                    Ok(Object::Array(array)) => Some(array),
                    _ => None,
                },
                _ => None,
            };
            if let Some(array) = array {
                // Operands may themselves be indirect. Like renderers, read
                // the first four numbers and ignore anything trailing.
                let values: Vec<f32> = array
                    .iter()
                    .filter_map(|value| match value {
                        Object::Reference(reference) => {
                            doc.get_object(*reference).ok().and_then(super::get_number)
                        }
                        _ => super::get_number(value),
                    })
                    .collect();
                if values.len() >= 4 {
                    if let Some(page_box) =
                        PageBox::from_corners(values[0], values[1], values[2], values[3])
                    {
                        return Some(page_box);
                    }
                }
            }
        }
        match dict.get(b"Parent") {
            Ok(Object::Reference(parent)) => id = *parent,
            _ => return None,
        }
    }
    None
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
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
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
