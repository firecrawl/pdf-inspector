//! Bounded content-stream decoding.
//!
//! `lopdf::content::Content::decode` materializes every operator before any
//! caller can apply a limit. A compact page of `q Q` pairs can therefore
//! allocate hundreds of megabytes and abort. Count operators first (without
//! allocating `Operation` objects) and skip decode when the cap is exceeded.

use crate::PdfError;
use lopdf::content::Content;

/// Maximum content-stream operators decoded for a page or a single Form
/// XObject. Matches the previous post-decode skip threshold.
pub(crate) const MAX_PAGE_OPERATIONS: usize = 1_000_000;

/// Maximum decompressed bytes read for a page's content, and for each Form
/// XObject read on its own, so a content bomb (a tiny Flate stream
/// inflating to gigabytes) skips the page or the form instead of exhausting
/// memory — the same degradation as the operator cap. Real page content
/// runs a few MB at most; the bound is deliberately far above that.
pub(crate) const MAX_PAGE_CONTENT_BYTES: usize = 64 * 1024 * 1024;

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

pub(super) fn is_content_space(b: u8) -> bool {
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

pub(super) fn is_inline_image_start(data: &[u8], i: usize) -> bool {
    data[i..].starts_with(b"BI")
        && data.get(i + 2).is_none_or(|&b| is_content_space(b))
        && (i == 0 || is_content_space(data[i - 1]) || matches!(data[i - 1], b')' | b'>' | b']'))
}

/// Offset just past the `EI` that closes the inline image whose `BI` ends at
/// `after_bi`. Unfiltered data is skipped by its declared length, as lopdf
/// reads it, so an `EI` lookalike among the pixel bytes does not end it early.
pub(super) fn inline_image_end(data: &[u8], after_bi: usize) -> Option<usize> {
    let id = after_bi + spaced_keyword_position(&data[after_bi..], b"ID")? + 1;
    let data_start = id + 3;
    let search_from = unfiltered_inline_image_len(&data[after_bi..id])
        .and_then(|len| data_start.checked_add(len))
        .filter(|&data_end| data_end <= data.len())
        .unwrap_or(data_start);
    let ei = search_from - 1 + spaced_keyword_position(&data[search_from - 1..], b"EI")? + 1;
    Some(ei + 2)
}

fn spaced_keyword_position(data: &[u8], keyword: &[u8; 2]) -> Option<usize> {
    data.windows(4)
        .position(|w| is_content_space(w[0]) && w[1..3] == keyword[..] && is_content_space(w[3]))
}

fn unfiltered_inline_image_len(header: &[u8]) -> Option<usize> {
    let mut tokens: Vec<&[u8]> = Vec::new();
    let mut start = 0;
    for (i, &b) in header.iter().enumerate() {
        if is_content_space(b) {
            tokens.push(&header[start..i]);
            start = i + 1;
        } else if b == b'/' {
            tokens.push(&header[start..i]);
            start = i;
        }
    }
    tokens.push(&header[start..]);
    tokens.retain(|token| !token.is_empty());

    let value = |abbreviation: &[u8], name: &[u8]| {
        let key = tokens
            .iter()
            .position(|&t| t == abbreviation || t == name)?;
        tokens.get(key + 1).copied()
    };
    let integer = |token: &[u8]| std::str::from_utf8(token).ok()?.parse::<usize>().ok();

    if value(b"/F", b"/Filter").is_some() {
        return None;
    }
    let width = integer(value(b"/W", b"/Width")?)?;
    let height = integer(value(b"/H", b"/Height")?)?;
    let bits_per_pixel = if value(b"/IM", b"/ImageMask") == Some(b"true") {
        1
    } else {
        let components: usize = match value(b"/CS", b"/ColorSpace")? {
            b"/G" | b"/DeviceGray" | b"/CalGray" => 1,
            b"/RGB" | b"/DeviceRGB" | b"/CalRGB" => 3,
            b"/CMYK" | b"/DeviceCMYK" => 4,
            _ => return None,
        };
        components.checked_mul(integer(value(b"/BPC", b"/BitsPerComponent")?)?)?
    };
    height.checked_mul(width.checked_mul(bits_per_pixel)?.div_ceil(8))
}

#[cfg(test)]
mod tests {
    use super::*;

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
