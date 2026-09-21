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
//! The repair is byte-level and offset-preserving: for each object the
//! cross-reference table lists but the loaded document lacks, the numerals
//! of its `/BBox` arrays that do not fit an `i64` are replaced, in place and
//! padded to their own length, by the extent a zero-area box is widened to
//! (see `form_bbox_repair`); the document is then loaded again from the
//! rewritten bytes. Objects that parsed are never touched, and neither is
//! stream data: the scan of an object stops at its `stream` keyword.

use std::collections::HashSet;

use lopdf::xref::XrefEntry;
use lopdf::Document;

use crate::form_bbox_repair::UNCLIPPED_FORM_BBOX_EXTENT;

/// Unloaded objects examined at most, and bytes read from each: the scan
/// repairs a handful of forms, it is not a second parser.
const MAX_OBJECTS_EXAMINED: usize = 256;
const MAX_OBJECT_SPAN: usize = 64 * 1024;

/// The bytes rewritten so that the `/BBox` numerals of unloaded objects fit
/// an `i64`, with the count of numerals rewritten, or `None` when no such
/// numeral was found. `doc` is the document lopdf loaded from `buffer`; the
/// offsets of its cross-reference table are read against `buffer`.
pub(crate) fn saturate_overlong_bbox_numerals(
    buffer: &[u8],
    doc: &Document,
) -> Option<(Vec<u8>, usize)> {
    let loaded: HashSet<u32> = doc.objects.keys().map(|id| id.0).collect();
    let mut out: Option<Vec<u8>> = None;
    let mut rewritten = 0usize;
    let mut examined = 0usize;
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
        if start >= buffer.len() {
            continue;
        }
        let span = &buffer[start..(start + MAX_OBJECT_SPAN).min(buffer.len())];
        let Some(header_len) = object_header_len(span, id) else {
            continue;
        };
        let dict_end = [&b"stream"[..], &b"endobj"[..]]
            .iter()
            .filter_map(|keyword| find(span, keyword))
            .min()
            .unwrap_or(span.len());
        if dict_end <= header_len {
            continue;
        }
        for (token_start, token_len, negative) in
            overlong_bbox_numerals(&span[header_len..dict_end])
        {
            let at = start + header_len + token_start;
            let bytes = out.get_or_insert_with(|| buffer.to_vec());
            let mut replacement = Vec::with_capacity(token_len);
            if negative {
                replacement.push(b'-');
            }
            replacement.extend_from_slice(UNCLIPPED_FORM_BBOX_EXTENT.to_string().as_bytes());
            replacement.resize(token_len, b' ');
            bytes[at..at + token_len].copy_from_slice(&replacement);
            rewritten += 1;
        }
    }
    out.map(|bytes| (bytes, rewritten))
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

/// The integer numerals inside the `/BBox [...]` arrays of `dict` that do
/// not fit an `i64`, as `(start, length, negative)` — the same test the
/// parser applies, so a numeral it would have read is left alone.
fn overlong_bbox_numerals(dict: &[u8]) -> Vec<(usize, usize, bool)> {
    let mut found = Vec::new();
    let mut pos = 0;
    while let Some(rel) = find(&dict[pos..], b"/BBox") {
        let after_key = pos + rel + b"/BBox".len();
        let open = after_key
            + dict[after_key..]
                .iter()
                .take_while(|b| b.is_ascii_whitespace())
                .count();
        if dict.get(open) != Some(&b'[') {
            pos = after_key;
            continue;
        }
        let Some(close) = find(&dict[open..], b"]") else {
            break;
        };
        let array = open + 1..open + close;
        let mut at = array.start;
        while at < array.end {
            let b = dict[at];
            if b == b'+' || b == b'-' || b.is_ascii_digit() {
                let token_start = at;
                let negative = b == b'-';
                if b == b'+' || b == b'-' {
                    at += 1;
                }
                let digits = dict[at..array.end]
                    .iter()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
                at += digits;
                let mut real = false;
                if dict.get(at) == Some(&b'.') {
                    real = true;
                    at += 1;
                    at += dict[at..array.end]
                        .iter()
                        .take_while(|b| b.is_ascii_digit())
                        .count();
                }
                let token = &dict[token_start..at];
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
        pos = array.end;
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
    pub(crate) fn pdf_with_form(bbox: &str, form_content: &str) -> Vec<u8> {
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
    pub(crate) fn unbounded() -> String {
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
        let dict = b"<< /BBox [ -1.5 2 99999999999999999999 ] /Matrix [ 99999999999999999999 0 0 1 0 0 ] >>";
        let found = overlong_bbox_numerals(dict);
        assert_eq!(
            found,
            vec![(18, 20, false)],
            "reals and the matrix are not read"
        );
    }
}
