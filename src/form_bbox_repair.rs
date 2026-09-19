//! Repair of Form XObjects whose `/BBox` has no area.
//!
//! A form XObject's `/BBox` clips its content. Some producers write a
//! zero-area box — `/BBox [0 0 0 0]` — on a form that holds a whole page's
//! content, typically when re-saving another producer's file, and draw the
//! page through it (`q 0 0 612 792 re W n /Fm1 Do Q`). Taken as the clip it
//! declares, the box hides the form entirely: a renderer paints nothing and
//! the page comes out blank, and an interpreter that honours the clip
//! extracts nothing. The producer plainly meant no clip at all, so such a
//! box is widened when the document is loaded to one that clips nothing —
//! what an interpreter skipping the clip for such a form would see. A box
//! with an area, however small, is a clip the producer meant and is left as
//! written.
//!
//! The repair changes the loaded objects, so it reaches every consumer of
//! the document: pdf-inspector's own extractors and, through
//! [`serialize_for_rendering`], the renderer of its OCR pipeline and any
//! caller that renders the document elsewhere.

use lopdf::{Document, Object, ObjectId};

/// Half-extent, in form space, of the box a zero-area `/BBox` is widened
/// to. Far outside any page or scaled form content, and a plain integer
/// every reader accepts.
pub(crate) const UNCLIPPED_FORM_BBOX_EXTENT: i64 = 1_000_000;

/// The `/BBox` that clips nothing.
fn unclipped_bbox() -> Object {
    Object::Array(vec![
        Object::Integer(-UNCLIPPED_FORM_BBOX_EXTENT),
        Object::Integer(-UNCLIPPED_FORM_BBOX_EXTENT),
        Object::Integer(UNCLIPPED_FORM_BBOX_EXTENT),
        Object::Integer(UNCLIPPED_FORM_BBOX_EXTENT),
    ])
}

/// Whether `object` is a Form XObject whose `/BBox` — direct or an indirect
/// reference — is four finite numbers spanning no width or no height.
/// Anything else, a box with an area, a malformed box or a stream of
/// another kind, is not repaired.
fn is_form_with_degenerate_bbox(doc: &Document, object: &Object) -> bool {
    let Object::Stream(stream) = object else {
        return false;
    };
    if stream
        .dict
        .get(b"Subtype")
        .ok()
        .and_then(|subtype| subtype.as_name().ok())
        != Some(b"Form")
    {
        return false;
    }
    let Ok(bbox) = stream.dict.get(b"BBox") else {
        return false;
    };
    let bbox = match bbox {
        Object::Reference(id) => match doc.get_object(*id) {
            Ok(resolved) => resolved,
            Err(_) => return false,
        },
        direct => direct,
    };
    let Ok(values) = bbox.as_array() else {
        return false;
    };
    if values.len() != 4 {
        return false;
    }
    let mut edges = [0f32; 4];
    for (edge, value) in edges.iter_mut().zip(values) {
        match value.as_float() {
            Ok(number) if number.is_finite() => *edge = number,
            _ => return false,
        }
    }
    let [x0, y0, x1, y1] = edges;
    x1 == x0 || y1 == y0
}

/// Widen the zero-area `/BBox` of every Form XObject in `doc` to a box that
/// clips nothing, and return how many forms were changed.
pub(crate) fn widen_degenerate_form_bboxes(doc: &mut Document) -> usize {
    let degenerate: Vec<ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, object)| is_form_with_degenerate_bbox(doc, object))
        .map(|(id, _)| *id)
        .collect();
    for id in &degenerate {
        if let Ok(Object::Stream(stream)) = doc.get_object_mut(*id) {
            stream.dict.set("BBox", unclipped_bbox());
        }
    }
    if !degenerate.is_empty() {
        log::debug!(
            "widened the zero-area BBox of {} form XObject(s)",
            degenerate.len()
        );
    }
    degenerate.len()
}

/// The document written back out for a renderer, once its forms have been
/// repaired: a plain serialization of the loaded objects. `None` for an
/// encrypted document, whose objects would have to be re-encrypted, and
/// when writing fails.
pub(crate) fn serialize_for_rendering(doc: &mut Document) -> Option<Vec<u8>> {
    if doc.is_encrypted() || doc.trailer.get(b"Encrypt").is_ok() {
        return None;
    }
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).ok()?;
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Stream};

    fn form_with_bbox(doc: &mut Document, bbox: Object) -> ObjectId {
        doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => bbox,
            },
            b"BT /F1 12 Tf 72 700 Td (form) Tj ET".to_vec(),
        )))
    }

    fn bbox_of(doc: &Document, id: ObjectId) -> Vec<f32> {
        let Ok(Object::Stream(stream)) = doc.get_object(id) else {
            panic!("no stream {id:?}");
        };
        stream
            .dict
            .get(b"BBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_float().unwrap())
            .collect()
    }

    fn numbers(values: &[i64]) -> Object {
        Object::Array(values.iter().map(|&v| Object::Integer(v)).collect())
    }

    #[test]
    fn zero_area_boxes_are_widened_and_others_left_alone() {
        let mut doc = Document::new();
        let empty = form_with_bbox(&mut doc, numbers(&[0, 0, 0, 0]));
        let no_width = form_with_bbox(&mut doc, numbers(&[10, 10, 10, 200]));
        let no_height = form_with_bbox(
            &mut doc,
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(20.5),
                Object::Real(612.0),
                Object::Real(20.5),
            ]),
        );
        let page_box = form_with_bbox(&mut doc, numbers(&[0, 0, 612, 792]));
        let thin = form_with_bbox(
            &mut doc,
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(0.01),
                Object::Real(792.0),
            ]),
        );
        let negative = form_with_bbox(&mut doc, numbers(&[612, 792, 0, 0]));

        assert_eq!(widen_degenerate_form_bboxes(&mut doc), 3);
        let wide = [
            -(UNCLIPPED_FORM_BBOX_EXTENT as f32),
            -(UNCLIPPED_FORM_BBOX_EXTENT as f32),
            UNCLIPPED_FORM_BBOX_EXTENT as f32,
            UNCLIPPED_FORM_BBOX_EXTENT as f32,
        ];
        assert_eq!(bbox_of(&doc, empty), wide);
        assert_eq!(bbox_of(&doc, no_width), wide);
        assert_eq!(bbox_of(&doc, no_height), wide);
        assert_eq!(bbox_of(&doc, page_box), [0.0, 0.0, 612.0, 792.0]);
        assert_eq!(bbox_of(&doc, thin), [0.0, 0.0, 0.01, 792.0]);
        assert_eq!(bbox_of(&doc, negative), [612.0, 792.0, 0.0, 0.0]);
        // A second pass finds nothing left to repair.
        assert_eq!(widen_degenerate_form_bboxes(&mut doc), 0);
    }

    #[test]
    fn an_indirect_zero_area_box_is_replaced_in_the_form() {
        let mut doc = Document::new();
        let bbox_id = doc.add_object(numbers(&[0, 0, 0, 0]));
        let form = form_with_bbox(&mut doc, Object::Reference(bbox_id));
        assert_eq!(widen_degenerate_form_bboxes(&mut doc), 1);
        assert_eq!(bbox_of(&doc, form)[2], UNCLIPPED_FORM_BBOX_EXTENT as f32);
        // The shared array itself is left as it was.
        assert_eq!(
            doc.get_object(bbox_id).unwrap().as_array().unwrap().len(),
            4
        );
    }

    #[test]
    fn malformed_boxes_and_other_streams_are_not_touched() {
        let mut doc = Document::new();
        let three = form_with_bbox(&mut doc, numbers(&[0, 0, 0]));
        let not_numbers = form_with_bbox(
            &mut doc,
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Name(b"zero".to_vec()),
                Object::Integer(0),
            ]),
        );
        let missing_reference = form_with_bbox(&mut doc, Object::Reference((999, 0)));
        let not_finite = form_with_bbox(
            &mut doc,
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(f32::NAN),
                Object::Real(0.0),
            ]),
        );
        let image = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 1,
                "Height" => 1,
                "BBox" => numbers(&[0, 0, 0, 0]),
            },
            vec![0],
        )));
        let pattern = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "PatternType" => 1,
                "BBox" => numbers(&[0, 0, 0, 0]),
            },
            Vec::new(),
        )));
        let no_bbox = doc.add_object(Object::Stream(Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Form" },
            Vec::new(),
        )));

        assert_eq!(widen_degenerate_form_bboxes(&mut doc), 0);
        assert_eq!(bbox_of(&doc, three).len(), 3);
        let Ok(Object::Stream(stream)) = doc.get_object(not_numbers) else {
            unreachable!()
        };
        assert!(matches!(
            stream.dict.get(b"BBox").unwrap().as_array().unwrap()[2],
            Object::Name(_)
        ));
        let Ok(Object::Stream(stream)) = doc.get_object(missing_reference) else {
            unreachable!()
        };
        assert!(matches!(stream.dict.get(b"BBox"), Ok(Object::Reference(_))));
        assert!(bbox_of(&doc, not_finite)[2].is_nan());
        assert_eq!(bbox_of(&doc, image), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(bbox_of(&doc, pattern), [0.0, 0.0, 0.0, 0.0]);
        let Ok(Object::Stream(stream)) = doc.get_object(no_bbox) else {
            unreachable!()
        };
        assert!(stream.dict.get(b"BBox").is_err());
    }

    #[test]
    fn serialization_skips_encrypted_documents() {
        let mut doc = Document::new();
        form_with_bbox(&mut doc, numbers(&[0, 0, 0, 0]));
        assert!(serialize_for_rendering(&mut doc).is_some());
        doc.trailer.set("Encrypt", Object::Reference((7, 0)));
        assert!(serialize_for_rendering(&mut doc).is_none());
    }
}
