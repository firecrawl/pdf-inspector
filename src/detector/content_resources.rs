//! What the names in a content stream resolve to — the XObject a `Do`
//! names, the pattern an `scn` names, each in the first of the resource
//! dictionaries in force that binds it — and what a stream's dictionary
//! says: its own resources, a pattern's type, a form's `/Matrix` and
//! `/BBox`.

use lopdf::{Document, Object, ObjectId};

/// What a `Do` operand names, in the first of the resources binding it.
pub(super) enum XObjectDrawn<'a> {
    Image,
    Form(ObjectId, &'a lopdf::Stream),
}

pub(super) fn resolve_xobject<'a>(
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
pub(super) fn stream_resources<'a>(
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
pub(super) fn resolve_pattern<'a>(
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
pub(super) fn pattern_type(doc: &Document, dict: &lopdf::Dictionary) -> Option<i64> {
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
pub(super) fn numbers_of<const N: usize>(
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
        *slot = match value {
            Object::Reference(id) => doc.get_object(*id).ok().and_then(coordinate),
            other => coordinate(other),
        }?;
    }
    Some(numbers)
}

/// A coordinate read at full precision: an integer straight into an `f64`
/// (a large one would lose digits through an `f32`), a real widened from
/// the single precision the file format gives it.
pub(super) fn coordinate(value: &Object) -> Option<f64> {
    match value {
        Object::Integer(n) => Some(*n as f64),
        Object::Real(r) => Some(f64::from(*r)),
        _ => None,
    }
}
