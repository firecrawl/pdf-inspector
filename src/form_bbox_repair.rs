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
/// reference, its numbers direct or indirect — is four finite numbers
/// spanning no width or no height. Anything else, a box with an area, a
/// malformed box or a stream of another kind, is not repaired.
fn is_form_with_degenerate_bbox(doc: &Document, object: &Object) -> bool {
    let Object::Stream(stream) = object else {
        return false;
    };
    let subtype = match stream.dict.get(b"Subtype") {
        Ok(Object::Reference(id)) => doc.get_object(*id).ok(),
        Ok(direct) => Some(direct),
        Err(_) => None,
    };
    if subtype.and_then(|subtype| subtype.as_name().ok()) != Some(b"Form") {
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
        let value = match value {
            Object::Reference(id) => match doc.get_object(*id) {
                Ok(resolved) => resolved,
                Err(_) => return false,
            },
            direct => direct,
        };
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
/// repaired: a plain serialization of the loaded objects. A document that
/// was loaded encrypted is written decrypted — the loader decrypted its
/// objects, and a renderer given these bytes reads them without the
/// password — so they are for a renderer in the same process, not for
/// keeping. `None` when the document is encrypted but its objects were not
/// decrypted, and when writing fails.
pub(crate) fn serialize_for_rendering(doc: &mut Document) -> Option<Vec<u8>> {
    if doc.is_encrypted() || doc.trailer.get(b"Encrypt").is_ok() {
        doc.encryption_state.as_ref()?;
        doc.trailer.remove(b"Encrypt");
        doc.encryption_state = None;
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
        // Indirect numbers inside the box, and an indirect `/Subtype`.
        let zero_id = doc.add_object(Object::Integer(0));
        let indirect_numbers = form_with_bbox(
            &mut doc,
            Object::Array(vec![
                Object::Reference(zero_id),
                Object::Integer(0),
                Object::Reference(zero_id),
                Object::Integer(792),
            ]),
        );
        let subtype_id = doc.add_object(Object::Name(b"Form".to_vec()));
        let indirect_subtype = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => Object::Reference(subtype_id),
                "BBox" => numbers(&[0, 0, 0, 0]),
            },
            Vec::new(),
        )));
        assert_eq!(widen_degenerate_form_bboxes(&mut doc), 3);
        for id in [form, indirect_numbers, indirect_subtype] {
            assert_eq!(bbox_of(&doc, id)[2], UNCLIPPED_FORM_BBOX_EXTENT as f32);
        }
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

    /// A one-page document drawn through a form with a zero-area box.
    fn page_through_degenerate_form() -> Document {
        let mut doc = Document::with_version("1.5");
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let form_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => numbers(&[0, 0, 0, 0]),
                "Resources" => dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font_id) } },
            },
            b"BT /F1 24 Tf 72 700 Td (Drawn through the form) Tj ET".to_vec(),
        )));
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            b"q 0 0 612 792 re W n /Fm1 Do Q".to_vec(),
        )));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => numbers(&[0, 0, 612, 792]),
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
                "XObject" => dictionary! { "Fm1" => Object::Reference(form_id) },
            },
            "Contents" => Object::Reference(content_id),
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let file_id = Object::string_literal(vec![0x42u8; 16]);
        doc.trailer
            .set("ID", Object::Array(vec![file_id.clone(), file_id]));
        doc
    }

    fn widened_form_count(doc: &Document) -> usize {
        doc.objects
            .values()
            .filter(|object| match object {
                Object::Stream(stream) => stream
                    .dict
                    .get(b"BBox")
                    .ok()
                    .and_then(|bbox| bbox.as_array().ok())
                    .is_some_and(|values| {
                        values[2].as_float().ok() == Some(UNCLIPPED_FORM_BBOX_EXTENT as f32)
                    }),
                _ => false,
            })
            .count()
    }

    /// A document loaded encrypted is written decrypted for the renderer;
    /// one whose objects were never decrypted is not written at all.
    #[test]
    fn serialization_writes_a_decrypted_document_and_skips_an_undecrypted_one() {
        let mut doc = page_through_degenerate_form();
        let state = lopdf::EncryptionState::try_from(lopdf::EncryptionVersion::V2 {
            document: &doc,
            owner_password: "owner",
            user_password: "",
            key_length: 128,
            permissions: lopdf::Permissions::all(),
        })
        .unwrap();
        doc.encrypt(&state).unwrap();
        let mut encrypted = Vec::new();
        doc.save_to(&mut encrypted).unwrap();

        let (mut loaded, _, repairs) =
            crate::load_document_from_mem_with_repairs(&encrypted, None).unwrap();
        assert_eq!(repairs.widened_form_bboxes, 1);
        assert!(loaded.encryption_state.is_some());
        let plain = serialize_for_rendering(&mut loaded).expect("a decrypted copy");
        let reloaded = Document::load_mem(&plain).unwrap();
        assert!(!reloaded.is_encrypted());
        assert_eq!(widened_form_count(&reloaded), 1);
        assert_eq!(
            crate::extract_text_with_positions_mem(&plain).unwrap()[0].text,
            "Drawn through the form"
        );

        let mut undecrypted = page_through_degenerate_form();
        undecrypted
            .trailer
            .set("Encrypt", Object::Reference((7, 0)));
        assert!(serialize_for_rendering(&mut undecrypted).is_none());
    }
}
