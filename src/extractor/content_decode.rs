//! Bounded content-stream decoding.
//!
//! `lopdf::content::Content::decode` materializes every operator before any
//! caller can apply a limit. A compact page of `q Q` pairs can therefore
//! allocate hundreds of megabytes and abort. Count operators first (without
//! allocating `Operation` objects) and skip decode when the cap is exceeded.

use crate::PdfError;
use lopdf::content::Content;
use lopdf::{DecompressError, Document, Error, Object, ObjectId, Stream};
use std::cell::Cell;

/// Maximum content-stream operators decoded for a page or a single Form
/// XObject. Matches the previous post-decode skip threshold.
pub(crate) const MAX_PAGE_OPERATIONS: usize = 1_000_000;

/// Maximum decompressed bytes read for a page's content, and for each Form
/// XObject read on its own, so a content bomb (a tiny Flate stream
/// inflating to gigabytes) skips the page or the form instead of exhausting
/// memory — the same degradation as the operator cap. Real page content
/// runs a few MB at most; the bound is deliberately far above that.
pub(crate) const MAX_PAGE_CONTENT_BYTES: usize = 64 * 1024 * 1024;

/// The least decompressed content-stream bytes a loaded document may decode
/// in all — every page's content and every form's, in detection and
/// extraction together, however often each is read.
const MIN_DOCUMENT_CONTENT_BYTES: usize = 1 << 30;

/// Decompressed content-stream bytes a document may decode per byte of its
/// file, above [`MIN_DOCUMENT_CONTENT_BYTES`]: a large document with much
/// content shared between pages reads it once per page and per pass, and
/// keeps room for that; a small file inflating to gigabytes does not.
const DOCUMENT_CONTENT_BYTES_PER_FILE_BYTE: usize = 512;

thread_local! {
    /// What is left of the content-stream budget of the document loaded
    /// last on this thread. Documents are read on the thread that loaded
    /// them, one at a time, from load to the end of the call.
    static DOCUMENT_CONTENT_BYTES_LEFT: Cell<usize> =
        const { Cell::new(MIN_DOCUMENT_CONTENT_BYTES) };
}

/// Start the content-stream budget of a document of `file_len` bytes just
/// loaded on this thread.
pub(crate) fn reset_document_content_budget(file_len: usize) {
    let budget = file_len
        .saturating_mul(DOCUMENT_CONTENT_BYTES_PER_FILE_BYTE)
        .max(MIN_DOCUMENT_CONTENT_BYTES);
    DOCUMENT_CONTENT_BYTES_LEFT.with(|left| left.set(budget));
}

/// A content stream's bytes decompressed within `limit` and within what is
/// left of the document's content budget, which they are charged to: a
/// stream over either is refused with `MemoryLimitExceeded`, and charged
/// the bytes read before it was refused. A stream that fails to decode for
/// another reason returns that error and is charged nothing, so a caller
/// may fall back to its raw bytes as before.
pub(crate) fn decompressed_content_within(stream: &Stream, limit: usize) -> lopdf::Result<Vec<u8>> {
    DOCUMENT_CONTENT_BYTES_LEFT.with(|left| {
        let available = left.get();
        let limit = limit.min(available);
        let result = stream.decompressed_content_with_limit(limit);
        let charged = match &result {
            Ok(content) => content.len(),
            Err(Error::Decompress(DecompressError::MemoryLimitExceeded { .. })) => limit,
            Err(_) => 0,
        };
        left.set(available.saturating_sub(charged));
        result
    })
}

/// A page's content streams decompressed and joined, as
/// `Document::get_page_content_with_limit` joins them, within `limit`
/// bytes for the page and within the document's content budget (see
/// [`decompressed_content_within`]). Refused with `MemoryLimitExceeded`
/// when the page's content would pass either.
pub(crate) fn page_content_within(
    doc: &Document,
    page_id: ObjectId,
    limit: usize,
) -> lopdf::Result<Vec<u8>> {
    let mut content = Vec::new();
    for id in doc.get_page_contents(page_id) {
        let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else {
            continue;
        };
        let remaining = limit.saturating_sub(content.len());
        match decompressed_content_within(stream, remaining) {
            Ok(data) => content.extend_from_slice(&data),
            Err(Error::Decompress(DecompressError::MemoryLimitExceeded { .. })) => {
                return Err(DecompressError::MemoryLimitExceeded { limit }.into());
            }
            Err(_) if stream.content.len() > remaining => {
                return Err(DecompressError::MemoryLimitExceeded { limit }.into());
            }
            Err(_) => content.extend_from_slice(&stream.content),
        }
        content.push(b'\n');
    }
    Ok(content)
}

/// Decode `data` unless it contains more than `max_operations` operators.
///
/// Returns `Ok(None)` when the stream exceeds the cap, so callers can skip
/// extraction without first allocating the operation vector.
pub(crate) fn decode_content_bounded(
    data: &[u8],
    max_operations: usize,
) -> Result<Option<Content>, PdfError> {
    if content_exceeds_operation_limit(data, max_operations) {
        return Ok(None);
    }
    Content::decode(data)
        .map(Some)
        .map_err(|e| PdfError::Parse(e.to_string()))
}

fn content_exceeds_operation_limit(data: &[u8], max_operations: usize) -> bool {
    count_content_operators(data, max_operations.saturating_add(1)) > max_operations
}

/// Count operators using the same token rules as lopdf's content parser,
/// stopping at `limit`. Does not allocate `Operation` / `Object` values.
fn count_content_operators(data: &[u8], limit: usize) -> usize {
    let mut i = 0;
    let mut count = 0;
    while i < data.len() && count < limit {
        skip_content_space(data, &mut i);
        if i >= data.len() {
            break;
        }
        if data[i] == b'%' {
            skip_comment(data, &mut i);
            continue;
        }
        match data[i] {
            b'(' => i = skip_literal_string(data, i),
            b'<' => {
                if data.get(i + 1) == Some(&b'<') {
                    i += 2;
                } else {
                    i = skip_hex_string(data, i);
                }
            }
            b'>' => {
                i += 1;
                if data.get(i) == Some(&b'>') {
                    i += 1;
                }
            }
            b'[' | b']' => i += 1,
            b'/' => skip_name(data, &mut i),
            b'+' | b'-' | b'.' => skip_number(data, &mut i),
            b if b.is_ascii_digit() => skip_number(data, &mut i),
            b if is_operator_byte(b) => {
                let start = i;
                i += 1;
                while i < data.len() && is_operator_byte(data[i]) {
                    i += 1;
                }
                let token = &data[start..i];
                if token == b"true" || token == b"false" || token == b"null" {
                    continue;
                }
                count += 1;
                if token == b"BI" && (i >= data.len() || is_content_space(data[i])) {
                    i = skip_inline_image_after_bi(data, i);
                }
            }
            _ => i += 1,
        }
    }
    count
}

fn is_content_space(b: u8) -> bool {
    // PDF whitespace (ISO 32000): NUL, tab, LF, FF, CR, space. Names must
    // stop on these so a following operator is not absorbed into `/Name`.
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_operator_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || matches!(b, b'*' | b'\'' | b'"')
}

fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn skip_content_space(data: &[u8], i: &mut usize) {
    while *i < data.len() && is_content_space(data[*i]) {
        *i += 1;
    }
}

fn skip_comment(data: &[u8], i: &mut usize) {
    while *i < data.len() && data[*i] != b'\n' && data[*i] != b'\r' {
        *i += 1;
    }
}

fn skip_literal_string(data: &[u8], mut i: usize) -> usize {
    let mut depth = 1i32;
    i += 1;
    while i < data.len() && depth > 0 {
        match data[i] {
            b'\\' => {
                i += 1;
                if i < data.len() {
                    i += 1;
                }
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    i
}

fn skip_hex_string(data: &[u8], mut i: usize) -> usize {
    i += 1;
    while i < data.len() && data[i] != b'>' {
        i += 1;
    }
    if i < data.len() {
        i += 1;
    }
    i
}

fn skip_name(data: &[u8], i: &mut usize) {
    *i += 1;
    while *i < data.len() && !is_content_space(data[*i]) && !is_delimiter(data[*i]) {
        *i += 1;
    }
}

fn skip_number(data: &[u8], i: &mut usize) {
    if *i < data.len() && matches!(data[*i], b'+' | b'-') {
        *i += 1;
    }
    while *i < data.len() && data[*i].is_ascii_digit() {
        *i += 1;
    }
    if *i < data.len() && data[*i] == b'.' {
        *i += 1;
        while *i < data.len() && data[*i].is_ascii_digit() {
            *i += 1;
        }
    }
}

/// After a `BI` operator, skip inline-image data through `EI`.
/// Uses the same PDF whitespace set as `is_content_space`. If `EI` is not
/// found, leave the cursor in place so later operators are still counted
/// (undercounting would let decode allocate the full vector).
fn skip_inline_image_after_bi(data: &[u8], mut i: usize) -> usize {
    skip_content_space(data, &mut i);
    let rest = &data[i..];
    if let Some(pos) = rest.windows(4).position(|w| {
        is_content_space(w[0]) && w[1] == b'E' && w[2] == b'I' && is_content_space(w[3])
    }) {
        return i + pos + 3;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    fn lopdf_op_count(data: &[u8]) -> usize {
        Content::decode(data)
            .map(|c| c.operations.len())
            .unwrap_or(0)
    }

    /// DoS safety: never report fewer operators than lopdf would allocate.
    /// Overcount is acceptable (skip a page); undercount would re-open decode.
    fn assert_count_does_not_undercount(data: &[u8]) {
        let ours = count_content_operators(data, usize::MAX);
        match Content::decode(data) {
            Ok(content) => assert!(
                ours >= content.operations.len(),
                "undercount: ours={ours} lopdf={} for {:?}",
                content.operations.len(),
                String::from_utf8_lossy(data)
            ),
            Err(_) => {}
        }
    }

    #[test]
    fn operator_count_matches_lopdf_for_typical_streams() {
        let samples: &[&[u8]] = &[
            b"q 1 0 0 1 0 0 cm BT /F1 12 Tf 72 720 Td (Hello) Tj ET Q",
            b"q Q q Q",
            b"BT /F1 12 Tf 12 TL 1 0 0 1 100 512 Tm (first) Tj (struck) ' ET",
            b"1 0 0 rg 0 0 10 10 re f",
            b"true false null q",
            b"% comment\nq Q\n",
            b"[ (a) 1 (b) ] TJ",
            b"1 0 0 1 0 0 cm /Im0 Do",
        ];
        for data in samples {
            assert_eq!(
                count_content_operators(data, usize::MAX),
                lopdf_op_count(data),
                "count mismatch for {}",
                String::from_utf8_lossy(data)
            );
        }
    }

    #[test]
    fn strings_and_comments_are_not_operators() {
        let data = b"(q Q Tj) Tj % q Q\nET";
        assert_eq!(
            count_content_operators(data, usize::MAX),
            lopdf_op_count(data)
        );
        assert_eq!(count_content_operators(data, usize::MAX), 2); // Tj, ET
    }

    #[test]
    fn inline_image_counts_as_one_operator() {
        let data = b"BI /W 2 /H 2 /CS /RGB /BPC 8 ID \x00\x01\x02\x03 EI q";
        assert_eq!(
            count_content_operators(data, usize::MAX),
            lopdf_op_count(data)
        );
        assert_eq!(count_content_operators(data, usize::MAX), 2); // BI, q
    }

    #[test]
    fn inline_image_ei_accepts_pdf_whitespace() {
        let tab = b"BI /W 1 /H 1 ID \xff\tEI\t q Q";
        let nul = b"BI /W 1 /H 1 ID \xff\x00EI\x00 q Q";
        let ff = b"BI /W 1 /H 1 ID \xff\x0cEI\x0c q Q";
        for data in [tab.as_slice(), nul.as_slice(), ff.as_slice()] {
            assert_count_does_not_undercount(data);
            assert!(
                count_content_operators(data, usize::MAX) >= 3,
                "BI plus following q Q must remain visible after EI, got {} for {:?}",
                count_content_operators(data, usize::MAX),
                String::from_utf8_lossy(data)
            );
        }
    }

    #[test]
    fn decode_is_skipped_when_operator_cap_is_exceeded() {
        let mut data = Vec::new();
        for _ in 0..20 {
            data.extend_from_slice(b"q Q\n");
        }
        assert!(decode_content_bounded(&data, 10).unwrap().is_none());
        let decoded = decode_content_bounded(&data, 50).unwrap().unwrap();
        assert_eq!(decoded.operations.len(), 40);
    }

    #[test]
    fn name_whitespace_does_not_swallow_following_operator() {
        // NUL / form-feed end a name (PDF whitespace). Absorbing `q` into
        // `/x` would undercount and let decode allocate the operator vector.
        let mut nul_sep = Vec::new();
        let mut ff_sep = Vec::new();
        for _ in 0..8_000 {
            nul_sep.extend_from_slice(b"/x\x00q");
            ff_sep.extend_from_slice(b"/x\x0cq");
        }
        assert_count_does_not_undercount(&nul_sep);
        assert_count_does_not_undercount(&ff_sep);
        assert!(count_content_operators(&ff_sep, usize::MAX) >= 8_000);
    }

    #[test]
    fn edge_streams_do_not_undercount_vs_lopdf() {
        let samples: &[&[u8]] = &[
            b".5 0 0 .5 0 0 cm",
            b"+1 -2 3.0 rg",
            b"<0041> Tj",
            b"(unbalanced",
            b"BI /W 1 /H 1 ID \xff\xff no EI here q Q q Q",
            b"q\x00Q\x00q\x00Q",
            b"/F1\x0c12 Tf (Hi) Tj",
            b"{ 1 2 add } cvx",
        ];
        for data in samples {
            assert_count_does_not_undercount(data);
        }
    }

    fn set_document_content_budget(bytes: usize) {
        DOCUMENT_CONTENT_BYTES_LEFT.with(|left| left.set(bytes));
    }

    fn document_content_budget() -> usize {
        DOCUMENT_CONTENT_BYTES_LEFT.with(Cell::get)
    }

    fn flate_stream(len: usize) -> Stream {
        let mut stream = Stream::new(dictionary! {}, vec![b' '; len]);
        stream.compress().unwrap();
        assert!(stream.dict.get(b"Filter").is_ok());
        stream
    }

    fn is_over_limit<T>(result: &lopdf::Result<T>) -> bool {
        matches!(
            result,
            Err(Error::Decompress(
                DecompressError::MemoryLimitExceeded { .. }
            ))
        )
    }

    /// A stream inflating past its limit is refused, and the document is
    /// charged what was read before it was; one within it is decoded and
    /// charged its length.
    #[test]
    fn stream_decode_is_bounded_and_charged_to_the_document() {
        set_document_content_budget(10_000);
        let stream = flate_stream(4_000);
        assert!(is_over_limit(&decompressed_content_within(&stream, 3_000)));
        assert_eq!(document_content_budget(), 7_000);
        assert_eq!(
            decompressed_content_within(&stream, 5_000).unwrap().len(),
            4_000
        );
        assert_eq!(document_content_budget(), 3_000);
        // Within its own limit, but past what the document has left.
        assert!(is_over_limit(&decompressed_content_within(&stream, 5_000)));
        assert_eq!(document_content_budget(), 0);
        assert!(is_over_limit(&decompressed_content_within(
            &Stream::new(dictionary! {}, b"q Q".to_vec()),
            5_000
        )));
    }

    /// The same stream read again and again — by every page that shares
    /// it — is refused once the document's budget is spent.
    #[test]
    fn a_shared_stream_read_over_and_over_spends_the_document_budget() {
        set_document_content_budget(1 << 20);
        let stream = flate_stream(100_000);
        let read = (0..100)
            .take_while(|_| decompressed_content_within(&stream, MAX_PAGE_CONTENT_BYTES).is_ok())
            .count();
        assert_eq!(read, 10);
    }

    /// The document budget grows with the file past its floor.
    #[test]
    fn document_budget_follows_the_file_size() {
        reset_document_content_budget(1_000);
        assert_eq!(document_content_budget(), MIN_DOCUMENT_CONTENT_BYTES);
        reset_document_content_budget(10 << 20);
        assert_eq!(
            document_content_budget(),
            (10 << 20) * DOCUMENT_CONTENT_BYTES_PER_FILE_BYTE
        );
    }

    fn doc_with_page_contents(streams: Vec<Stream>) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let contents: Vec<Object> = streams
            .into_iter()
            .map(|stream| doc.add_object(stream).into())
            .collect();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => contents,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        (doc, page_id)
    }

    /// A page's streams are joined as lopdf joins them, within the page's
    /// limit and the document's budget.
    #[test]
    fn page_content_is_joined_within_the_page_limit_and_document_budget() {
        let (doc, page_id) = doc_with_page_contents(vec![
            Stream::new(dictionary! {}, b"BT (a) Tj ET".to_vec()),
            flate_stream(2_000),
            Stream::new(
                dictionary! { "Filter" => "FlateDecode" },
                b"not deflate".to_vec(),
            ),
        ]);
        set_document_content_budget(1 << 20);
        let expected = doc.get_page_content_with_limit(page_id, 1 << 20).unwrap();
        assert_eq!(
            page_content_within(&doc, page_id, 1 << 20).unwrap(),
            expected
        );
        assert!(is_over_limit(&page_content_within(&doc, page_id, 1_000)));

        set_document_content_budget(1_500);
        assert!(is_over_limit(&page_content_within(&doc, page_id, 1 << 20)));
    }

    #[test]
    fn million_q_pairs_are_rejected_without_decode() {
        let mut data = Vec::with_capacity((MAX_PAGE_OPERATIONS + 1) * 2);
        for _ in 0..=MAX_PAGE_OPERATIONS {
            data.extend_from_slice(b"q\n");
        }
        assert!(content_exceeds_operation_limit(&data, MAX_PAGE_OPERATIONS));
        assert!(decode_content_bounded(&data, MAX_PAGE_OPERATIONS)
            .unwrap()
            .is_none());
    }
}
