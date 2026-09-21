//! Repair of Form XObjects whose `/BBox` holds numerals no parser can hold.
//!
//! One re-save pattern wraps a page's content in a Form XObject and writes
//! the form's `/BBox` as four integers of 308 digits — ±(DBL_MAX / 2)
//! written out in full, meaning "unbounded". A 64-bit integer parser cannot
//! hold them, so the whole object fails to parse and is dropped: the page's
//! `Do` then draws nothing and the page comes out empty. Readers that
//! overflow the numeral to zero clip the form to nothing instead, and the
//! page renders blank. Both read the file as meant once the numerals are a
//! box that clips nothing.
//!
//! The repair is byte-level and offset-preserving. For each object the
//! cross-reference table lists but the loaded document lacks, the object's
//! dictionary is read with comments and strings masked out; when it is a
//! Form XObject (`/Subtype /Form`), the numerals of its `/BBox` array that
//! do not fit an `i64` are replaced, in place and padded to their own
//! length, by the extent a zero-area box is widened to (see
//! `form_bbox_repair`) — in the dictionary itself, or in the array object a
//! `/BBox n 0 R` refers to (whether or not the form's own dictionary
//! parsed). The document is then loaded again from the
//! rewritten bytes. Objects that parsed are never touched, nor is anything
//! outside those dictionaries and arrays: not stream data, not strings, not
//! comments, not the box of a pattern or a shading.

use std::collections::HashSet;
use std::ops::Range;

use lopdf::xref::XrefEntry;
use lopdf::Document;

use crate::form_bbox_repair::UNCLIPPED_FORM_BBOX_EXTENT;

/// Unloaded objects examined at most, and bytes read from each: the scan
/// repairs a handful of forms, it is not a second parser.
const MAX_OBJECTS_EXAMINED: usize = 256;
const MAX_OBJECT_SPAN: usize = 64 * 1024;

/// Where an unloaded form keeps its `/BBox`.
enum BBoxValue {
    /// The array is inline: its body, as a range into the object span.
    Inline(Range<usize>),
    /// The array is an indirect object with this number.
    Reference(u32),
}

/// The bytes rewritten so that the `/BBox` numerals of unloaded Form
/// XObjects fit an `i64`, with the count of numerals rewritten, or `None`
/// when no such numeral was found. `doc` is the document lopdf loaded from
/// `buffer`; the offsets of its cross-reference table are read against
/// `buffer`.
pub(crate) fn saturate_overlong_bbox_numerals(
    buffer: &[u8],
    doc: &Document,
) -> Option<(Vec<u8>, usize)> {
    let loaded: HashSet<u32> = doc.objects.keys().map(|id| id.0).collect();
    let offset_of = |id: u32| match doc.reference_table.get(id) {
        Some(XrefEntry::Normal { offset, .. }) => Some(*offset as usize),
        _ => None,
    };
    let mut out: Option<Vec<u8>> = None;
    let mut rewritten = 0usize;
    let mut examined = 0usize;
    let mut rewrite = |out: &mut Option<Vec<u8>>, at: usize, len: usize, negative: bool| {
        let bytes = out.get_or_insert_with(|| buffer.to_vec());
        let mut replacement = Vec::with_capacity(len);
        if negative {
            replacement.push(b'-');
        }
        replacement.extend_from_slice(UNCLIPPED_FORM_BBOX_EXTENT.to_string().as_bytes());
        replacement.resize(len, b' ');
        bytes[at..at + len].copy_from_slice(&replacement);
        rewritten += 1;
    };

    // The forms first: their inline boxes are repaired here, their
    // referenced boxes remembered. A form that parsed (its dictionary holds
    // only `/BBox n 0 R`) may still point at an array object that did not.
    let mut referenced: Vec<u32> = doc
        .objects
        .values()
        .filter_map(|object| match object {
            lopdf::Object::Stream(stream)
                if stream.dict.get(b"Subtype").ok()
                    == Some(&lopdf::Object::Name(b"Form".to_vec())) =>
            {
                match stream.dict.get(b"BBox") {
                    Ok(lopdf::Object::Reference(id)) if !loaded.contains(&id.0) => Some(id.0),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    for (&id, entry) in &doc.reference_table.entries {
        let XrefEntry::Normal { offset, .. } = entry else {
            continue;
        };
        if loaded.contains(&id) {
            continue;
        }
        examined += 1;
        if examined > MAX_OBJECTS_EXAMINED {
            break;
        }
        let start = *offset as usize;
        let Some((span, masked, header_len)) = object_at(buffer, start, id) else {
            continue;
        };
        let Some(dict) = dictionary_range(&masked, header_len) else {
            continue;
        };
        if !names_form(&masked[dict.clone()]) {
            continue;
        }
        match bbox_value(&masked, dict) {
            Some(BBoxValue::Inline(array)) => {
                for (token_start, token_len, negative) in overlong_numerals(&span[array.clone()]) {
                    rewrite(
                        &mut out,
                        start + array.start + token_start,
                        token_len,
                        negative,
                    );
                }
            }
            Some(BBoxValue::Reference(number)) => referenced.push(number),
            None => {}
        }
    }
    // Then the array objects those forms refer to.
    for number in referenced {
        if loaded.contains(&number) {
            continue;
        }
        examined += 1;
        if examined > MAX_OBJECTS_EXAMINED {
            break;
        }
        let Some(start) = offset_of(number) else {
            continue;
        };
        let Some((span, masked, header_len)) = object_at(buffer, start, number) else {
            continue;
        };
        let Some(array) = array_range(&masked, header_len) else {
            continue;
        };
        for (token_start, token_len, negative) in overlong_numerals(&span[array.clone()]) {
            rewrite(
                &mut out,
                start + array.start + token_start,
                token_len,
                negative,
            );
        }
    }
    out.map(|bytes| (bytes, rewritten))
}

/// The object `id` at `start`: its span (at most [`MAX_OBJECT_SPAN`]), the
/// same bytes with comments and strings masked, and the length of its
/// `<id> <generation> obj` header.
fn object_at(buffer: &[u8], start: usize, id: u32) -> Option<(&[u8], Vec<u8>, usize)> {
    if start >= buffer.len() {
        return None;
    }
    let span = &buffer[start..(start + MAX_OBJECT_SPAN).min(buffer.len())];
    let header_len = object_header_len(span, id)?;
    Some((span, mask_noise(span), header_len))
}

/// `span` with every byte of a comment, a literal string or a hex string
/// replaced by a space, so that the delimiters and names found afterwards
/// are the syntax's own. Dictionary delimiters `<<` and `>>` are kept.
fn mask_noise(span: &[u8]) -> Vec<u8> {
    let mut out = span.to_vec();
    let mut i = 0;
    while i < span.len() {
        match span[i] {
            b'%' => {
                while i < span.len() && span[i] != b'\n' && span[i] != b'\r' {
                    out[i] = b' ';
                    i += 1;
                }
            }
            b'(' => {
                let mut depth = 0usize;
                while i < span.len() {
                    let b = span[i];
                    out[i] = b' ';
                    i += 1;
                    match b {
                        b'\\' => {
                            if i < span.len() {
                                out[i] = b' ';
                                i += 1;
                            }
                        }
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            b'<' if span.get(i + 1) != Some(&b'<') => {
                while i < span.len() {
                    let b = span[i];
                    out[i] = b' ';
                    i += 1;
                    if b == b'>' {
                        break;
                    }
                }
            }
            b'<' => i += 2,
            b'>' if span.get(i + 1) == Some(&b'>') => i += 2,
            _ => i += 1,
        }
    }
    out
}

/// The body of the first dictionary opening at or after `from` in `masked`
/// — the bytes between its `<<` and its matching `>>` — or `None` when
/// none opens or none closes within the span.
fn dictionary_range(masked: &[u8], from: usize) -> Option<Range<usize>> {
    let open = from + find(&masked[from..], b"<<")?;
    let mut depth = 0usize;
    let mut i = open;
    while i + 1 < masked.len() {
        if &masked[i..i + 2] == b"<<" {
            depth += 1;
            i += 2;
        } else if &masked[i..i + 2] == b">>" {
            depth -= 1;
            if depth == 0 {
                return Some(open + 2..i);
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    None
}

/// The body of the first array opening at or after `from` in `masked` —
/// the bytes between its `[` and its matching `]`.
fn array_range(masked: &[u8], from: usize) -> Option<Range<usize>> {
    let open = from + masked[from..].iter().position(|b| *b == b'[')?;
    let mut depth = 0usize;
    for (i, b) in masked.iter().enumerate().skip(open) {
        match b {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + 1..i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether a masked dictionary body names a Form XObject: a `/Subtype` key
/// followed by the name `/Form`.
fn names_form(dict: &[u8]) -> bool {
    let mut pos = 0;
    while let Some(rel) = find_name(&dict[pos..], b"/Subtype") {
        let after = pos + rel + b"/Subtype".len();
        let value = after
            + dict[after..]
                .iter()
                .take_while(|b| b.is_ascii_whitespace())
                .count();
        if dict[value..].starts_with(b"/Form") && !dict.get(value + 5).is_some_and(is_regular) {
            return true;
        }
        pos = after;
    }
    false
}

/// The `/BBox` value of a masked dictionary body `dict` (a range into the
/// whole span): the inline array's body, or the number of the object it
/// refers to.
fn bbox_value(masked: &[u8], dict: Range<usize>) -> Option<BBoxValue> {
    let body = &masked[dict.clone()];
    let rel = find_name(body, b"/BBox")?;
    let after = dict.start + rel + b"/BBox".len();
    let value = after
        + masked[after..dict.end]
            .iter()
            .take_while(|b| b.is_ascii_whitespace())
            .count();
    if masked.get(value) == Some(&b'[') {
        let array = array_range(masked, value)?;
        return (array.end <= dict.end).then_some(BBoxValue::Inline(array));
    }
    // `<number> <generation> R`
    let mut pos = value;
    let digits = masked[pos..dict.end]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digits == 0 {
        return None;
    }
    let number: u32 = std::str::from_utf8(&masked[pos..pos + digits])
        .ok()?
        .parse()
        .ok()?;
    pos += digits;
    let space = masked[pos..dict.end]
        .iter()
        .take_while(|b| b.is_ascii_whitespace())
        .count();
    if space == 0 {
        return None;
    }
    pos += space;
    let generation = masked[pos..dict.end]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if generation == 0 {
        return None;
    }
    pos += generation;
    pos += masked[pos..dict.end]
        .iter()
        .take_while(|b| b.is_ascii_whitespace())
        .count();
    (masked.get(pos) == Some(&b'R') && !masked.get(pos + 1).is_some_and(is_regular))
        .then_some(BBoxValue::Reference(number))
}

/// A byte that continues a name or a keyword: neither white space nor a
/// PDF delimiter.
fn is_regular(b: &u8) -> bool {
    !b.is_ascii_whitespace()
        && !matches!(
            b,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

/// The position of the name `name` (with its leading slash) in `haystack`
/// as a whole token: followed by white space or a delimiter, not by more
/// regular characters.
fn find_name(haystack: &[u8], name: &[u8]) -> Option<usize> {
    let mut pos = 0;
    while let Some(rel) = find(&haystack[pos..], name) {
        let at = pos + rel;
        if !haystack.get(at + name.len()).is_some_and(is_regular) {
            return Some(at);
        }
        pos = at + 1;
    }
    None
}

/// The length of the `<id> <generation> obj` header that opens `span`,
/// after any leading white space, when `span` holds object `id`.
fn object_header_len(span: &[u8], id: u32) -> Option<usize> {
    let mut pos = span.iter().position(|b| !b.is_ascii_whitespace())?;
    let expected = id.to_string();
    if !span[pos..].starts_with(expected.as_bytes()) {
        return None;
    }
    pos += expected.len();
    let after_id = pos;
    pos += span[pos..]
        .iter()
        .take_while(|b| b.is_ascii_whitespace())
        .count();
    if pos == after_id {
        return None;
    }
    let generation = span[pos..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if generation == 0 {
        return None;
    }
    pos += generation;
    pos += span[pos..]
        .iter()
        .take_while(|b| b.is_ascii_whitespace())
        .count();
    span[pos..].starts_with(b"obj").then_some(pos + 3)
}

/// The integer numerals in `array` (an array body, strings and comments
/// already masked away by the caller) that do not fit an `i64`, as
/// `(start, length, negative)` — the same test the parser applies, so a
/// numeral it would have read is left alone.
fn overlong_numerals(array: &[u8]) -> Vec<(usize, usize, bool)> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < array.len() {
        let b = array[at];
        if b == b'+' || b == b'-' || b.is_ascii_digit() {
            let token_start = at;
            let negative = b == b'-';
            if b == b'+' || b == b'-' {
                at += 1;
            }
            let digits = array[at..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            at += digits;
            let mut real = false;
            if array.get(at) == Some(&b'.') {
                real = true;
                at += 1;
                at += array[at..]
                    .iter()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
            }
            let token = &array[token_start..at];
            if !real
                && digits > 0
                && std::str::from_utf8(token)
                    .ok()
                    .and_then(|t| t.parse::<i64>().ok())
                    .is_none()
            {
                found.push((token_start, token.len(), negative));
            }
        } else {
            at += 1;
        }
    }
    found
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF drawing `Fm1`, a Form XObject declaring `bbox` and
    /// holding `form_content`, written by hand so the box can hold what
    /// lopdf cannot.
    fn pdf_with_form(bbox: &str, form_content: &str) -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> \
             /XObject << /Fm1 6 0 R >> >> /Contents 4 0 R >>"
                .to_string(),
            "<< /Length 35 >>\nstream\nq Q q 0 0 612 792 re W n /Fm1 Do Q\nendstream".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!(
                "<< /Type /XObject /Subtype /Form /BBox [{bbox}] /Resources << /Font << /F1 5 0 R >> >> \
                 /Length {} >>\nstream\n{form_content}\nendstream",
                form_content.len()
            ),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    /// ±(DBL_MAX / 2) as a re-save writes it: 308 digits.
    fn unbounded() -> String {
        let digits = format!("8988465674311578{}", "0".repeat(292));
        format!("-{digits} -{digits} {digits} {digits}")
    }

    const TEXT: &str = "BT /F1 24 Tf 72 700 Td (Drawn through the form) Tj ET";

    #[test]
    fn the_unloaded_forms_numerals_are_saturated_in_place() {
        let bytes = pdf_with_form(&unbounded(), TEXT);
        let doc = Document::load_mem(&bytes).unwrap();
        assert!(
            !doc.objects.keys().any(|id| id.0 == 6),
            "lopdf drops the form as written"
        );
        let (repaired, count) = saturate_overlong_bbox_numerals(&bytes, &doc).unwrap();
        assert_eq!(count, 4);
        assert_eq!(repaired.len(), bytes.len(), "offsets are preserved");
        let differing = bytes.iter().zip(&repaired).filter(|(a, b)| a != b).count();
        assert!(differing <= 4 * 308, "{differing} bytes changed");
        let reloaded = Document::load_mem(&repaired).unwrap();
        let form = reloaded.get_object((6, 0)).unwrap().as_stream().unwrap();
        let bbox: Vec<i64> = form
            .dict
            .get(b"BBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_i64().unwrap())
            .collect();
        let e = UNCLIPPED_FORM_BBOX_EXTENT;
        assert_eq!(bbox, vec![-e, -e, e, e]);
        assert_eq!(form.content, TEXT.as_bytes());
    }

    #[test]
    fn a_form_that_parses_and_stream_data_are_left_alone() {
        // A box any parser reads: nothing to do, whatever the stream holds.
        let long_run_in_stream = format!("{TEXT} % {}", "7".repeat(40));
        let bytes = pdf_with_form("0 0 612 792", &long_run_in_stream);
        let doc = Document::load_mem(&bytes).unwrap();
        assert!(saturate_overlong_bbox_numerals(&bytes, &doc).is_none());
        // The largest i64 still parses and is kept; one more digit does not.
        let bytes = pdf_with_form("0 0 9223372036854775807 92233720368547758070", TEXT);
        let doc = Document::load_mem(&bytes).unwrap();
        let (repaired, count) = saturate_overlong_bbox_numerals(&bytes, &doc).unwrap();
        assert_eq!(count, 1);
        assert!(find(&repaired, b"9223372036854775807 1000000").is_some());
        // An unloaded form's stream is not scanned: a long digit run there stays.
        let bytes = pdf_with_form(&unbounded(), &long_run_in_stream);
        let doc = Document::load_mem(&bytes).unwrap();
        let (repaired, _) = saturate_overlong_bbox_numerals(&bytes, &doc).unwrap();
        assert!(find(&repaired, "7".repeat(40).as_bytes()).is_some());
    }

    #[test]
    fn header_and_numeral_scanners_read_what_they_should() {
        assert_eq!(object_header_len(b"\n6 0 obj\n<<", 6), Some(8));
        assert_eq!(object_header_len(b"16 0 obj", 6), None);
        assert_eq!(object_header_len(b"6 0 endobj", 6), None);
        // Reals are not read; neither is anything but the box's array.
        assert_eq!(
            overlong_numerals(b" -1.5 2 99999999999999999999 "),
            vec![(8, 20, false)]
        );
        // A string (with a nested pair and an escaped delimiter), a hex
        // string and a comment are blanked; the dictionary delimiters and
        // the real key are kept, and the length does not change.
        let raw = b"<< /A (a (nested) \\) % /BBox [ 1 ]) /B <414243> % /BBox [ 2 ]\n/BBox [ 3 ] >>";
        let masked = mask_noise(raw);
        assert_eq!(masked.len(), raw.len());
        let shown = String::from_utf8_lossy(&masked).into_owned();
        assert!(find(&masked, b"/BBox [ 1 ]").is_none(), "{shown}");
        assert!(find(&masked, b"/BBox [ 2 ]").is_none(), "{shown}");
        assert!(find(&masked, b"414243").is_none(), "{shown}");
        assert!(
            masked.starts_with(b"<< /A ") && masked.ends_with(b">>"),
            "{shown}"
        );
        let dict = dictionary_range(&masked, 0).unwrap();
        let Some(BBoxValue::Inline(array)) = bbox_value(&masked, dict) else {
            panic!("the real box is read: {shown}");
        };
        assert_eq!(&masked[array], b" 3 ");
        assert_eq!(find_name(b"/BBoxes 1 /BBox [", b"/BBox"), Some(10));
        assert!(names_form(b" /Type /XObject /Subtype /Form "));
        assert!(!names_form(b" /Subtype /Formula "));
        assert!(!names_form(b" /Subtype /Image "));
    }

    /// The box's array in a string or a comment is not the box; a key that
    /// spells `stream` before the box does not end the dictionary; a box
    /// held in an indirect array object is repaired there; a pattern's box
    /// is not a form's and is left alone.
    #[test]
    fn only_a_forms_own_box_is_read_and_it_may_be_indirect() {
        let bbox = unbounded();
        // Decoys in a string and a comment, and a key spelling `stream`,
        // all before the real box.
        let form = format!(
            "<< /Type /XObject /Subtype /Form /Note (/BBox [ {bbox} ]) % /BBox [ {bbox} ]\n\
             /streamparams 1 /BBox [{bbox}] /Resources << /Font << /F1 5 0 R >> >> /Length {} >>\nstream\n{TEXT}\nendstream",
            TEXT.len()
        );
        let bytes = pdf_with_objects(&form, None);
        let doc = Document::load_mem(&bytes).unwrap();
        let (repaired, count) = saturate_overlong_bbox_numerals(&bytes, &doc).unwrap();
        assert_eq!(count, 4, "only the box's own four numerals");
        assert_eq!(repaired.len(), bytes.len());
        let reloaded = Document::load_mem(&repaired).unwrap();
        let form = reloaded.get_object((6, 0)).unwrap().as_stream().unwrap();
        assert_eq!(form.dict.get(b"BBox").unwrap().as_array().unwrap().len(), 4);
        // The string and the comment kept their digits.
        assert!(find(&repaired, format!("(/BBox [ {bbox} ])").as_bytes()).is_some());

        // The box as an indirect array object.
        let form = format!(
            "<< /Type /XObject /Subtype /Form /BBox 7 0 R /Resources << /Font << /F1 5 0 R >> >> /Length {} >>\nstream\n{TEXT}\nendstream",
            TEXT.len()
        );
        let bytes = pdf_with_objects(&form, Some(&format!("[ {bbox} ]")));
        let doc = Document::load_mem(&bytes).unwrap();
        assert!(
            !doc.objects.keys().any(|id| id.0 == 7),
            "the array object is dropped as written"
        );
        let (repaired, count) = saturate_overlong_bbox_numerals(&bytes, &doc).unwrap();
        assert_eq!(count, 4);
        let reloaded = Document::load_mem(&repaired).unwrap();
        let e = UNCLIPPED_FORM_BBOX_EXTENT;
        let array: Vec<i64> = reloaded
            .get_object((7, 0))
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_i64().unwrap())
            .collect();
        assert_eq!(array, vec![-e, -e, e, e]);
        assert!(
            reloaded.get_object((6, 0)).is_ok(),
            "the form loads with its box"
        );

        // A tiling pattern with the same box is not a Form XObject.
        let pattern = format!(
            "<< /Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [{bbox}] /XStep 10 /YStep 10 \
             /Resources << >> /Length {} >>\nstream\n{TEXT}\nendstream",
            TEXT.len()
        );
        let bytes = pdf_with_objects(&pattern, None);
        let doc = Document::load_mem(&bytes).unwrap();
        assert!(saturate_overlong_bbox_numerals(&bytes, &doc).is_none());
    }

    /// A one-page PDF whose object 6 is `object6` (a form or a pattern with
    /// a stream) and whose object 7, when given, is `object7`.
    fn pdf_with_objects(object6: &str, object7: Option<&str>) -> Vec<u8> {
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> \
             /XObject << /Fm1 6 0 R >> >> /Contents 4 0 R >>"
                .to_string(),
            "<< /Length 35 >>\nstream\nq Q q 0 0 612 792 re W n /Fm1 Do Q\nendstream".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            object6.to_string(),
        ];
        if let Some(object7) = object7 {
            objects.push(object7.to_string());
        }
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }
}
