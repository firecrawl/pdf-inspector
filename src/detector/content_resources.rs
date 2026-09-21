//! What the names in a content stream resolve to — the XObject a `Do`
//! names, the pattern an `scn` names, each in the first of the resource
//! dictionaries in force that binds it — what a stream's dictionary says:
//! its own resources, a pattern's type, a form's `/Matrix` and `/BBox`,
//! the components of a colour space it names — and a stream's content,
//! decoded within a limit.

use super::content_mask::device_colour_space_components;
use lopdf::{DecompressError, Document, Error, Object, ObjectId};

/// How many components the colour space `name` names in the first of
/// `resources` binding it has — for the length of an inline image's data
/// drawn in that space: a device or CIE-based space by its family,
/// `/ICCBased` by its stream's `/N`, `/Indexed` and `/Separation` one,
/// `/DeviceN` as many as its names. `None` when the name is unbound or the
/// space is of no family known here.
pub(super) fn colour_space_components(
    doc: &Document,
    resources: &[&lopdf::Dictionary],
    name: &[u8],
) -> Option<u32> {
    for scope in resources {
        let spaces = match scope.get(b"ColorSpace").ok() {
            Some(Object::Dictionary(dict)) => dict,
            Some(Object::Reference(id)) => match doc.get_dictionary(*id) {
                Ok(dict) => dict,
                Err(_) => continue,
            },
            _ => continue,
        };
        let Ok(space) = spaces.get(name) else {
            continue;
        };
        return components_of_colour_space(doc, space, 0);
    }
    None
}

/// The components of the colour space object `space`: a name, or an array
/// led by its family's name — either direct or by reference.
fn components_of_colour_space(doc: &Document, space: &Object, depth: u8) -> Option<u32> {
    if depth > 4 {
        return None;
    }
    match space {
        Object::Reference(id) => {
            components_of_colour_space(doc, doc.get_object(*id).ok()?, depth + 1)
        }
        Object::Name(name) => device_colour_space_components(name),
        Object::Array(items) => {
            let family = match items.first()? {
                Object::Reference(id) => doc.get_object(*id).ok()?.as_name().ok()?,
                other => other.as_name().ok()?,
            };
            match family {
                b"ICCBased" => {
                    let stream = match items.get(1)? {
                        Object::Reference(id) => doc.get_object(*id).ok()?,
                        other => other,
                    };
                    let n = stream.as_stream().ok()?.dict.get(b"N").ok()?;
                    let n = match n {
                        Object::Reference(id) => doc.get_object(*id).ok()?.as_i64().ok()?,
                        other => other.as_i64().ok()?,
                    };
                    u32::try_from(n).ok().filter(|n| matches!(n, 1 | 3 | 4))
                }
                b"Indexed" | b"I" | b"Separation" => Some(1),
                b"DeviceN" => {
                    let names = match items.get(1)? {
                        Object::Reference(id) => doc.get_object(*id).ok()?.as_array().ok()?,
                        other => other.as_array().ok()?,
                    };
                    u32::try_from(names.len()).ok()
                }
                b"Pattern" => None,
                other => device_colour_space_components(other),
            }
        }
        _ => None,
    }
}

/// A stream's content decoded within `limit` bytes — a form's or a
/// pattern cell's, for a scan that may execute no more than that: `None`
/// when it would take more. A stream without a `/Filter` is its bytes,
/// refused by their length before any copy; a filtered one is decoded by
/// the bounded decoder, which reads no further than the limit before
/// refusing it; one that cannot be decoded is read as its raw bytes, as
/// the unbounded reading read it, when they fit.
pub(super) fn decoded_within(stream: &lopdf::Stream, limit: usize) -> Option<Vec<u8>> {
    let raw_within = || (stream.content.len() <= limit).then(|| stream.content.clone());
    if stream.dict.get(b"Filter").is_err() {
        return raw_within();
    }
    match stream.decompressed_content_with_limit(limit) {
        Ok(content) => Some(content),
        Err(Error::Decompress(DecompressError::MemoryLimitExceeded { .. })) => None,
        Err(_) => raw_within(),
    }
}

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
