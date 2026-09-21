//! What the executed-content scan reads operators through: a copy of the
//! stream with its strings, comments and inline image data blanked, and
//! the operand lookbacks — numbers, a name, how much text a show operator
//! has to show — that the scan runs on it. Whitespace throughout is the
//! file format's: NUL, TAB, LF, FF, CR and SPACE.

use super::{is_pdf_name_delimiter, is_pdf_whitespace};

/// Whether `byte` ends the operator token before it: whitespace, or an
/// opening delimiter, since a stream may run one token into the next
/// (`Tf[`, `Tj(`, `BI/W`).
fn ends_token(byte: u8) -> bool {
    is_pdf_whitespace(byte) || matches!(byte, b'/' | b'[' | b'(' | b'<' | b'%')
}

/// `content` with everything that is not an operator or its operands
/// blanked to spaces, at the same offsets: the insides of literal strings
/// (nesting and escapes honoured), of hex strings and of comments, and
/// inline image data, from the `ID` of an inline image `BI` opened through
/// its `EI`. The delimiters stay, so a string still closes an operand; the
/// strings' bytes are read from the original when a text operator is
/// found.
pub(super) fn mask_strings_comments_and_inline_images(content: &[u8]) -> Vec<u8> {
    /// Whether an operator token may begin at `i`: at the start, or after
    /// whitespace or a closing delimiter.
    fn after_token_break(content: &[u8], i: usize) -> bool {
        i == 0 || is_pdf_whitespace(content[i - 1]) || matches!(content[i - 1], b')' | b']' | b'>')
    }
    let mut masked = content.to_vec();
    // `ID` begins image data only inside an inline image, which `BI` opens
    // (its header follows, from this offset); anywhere else — a bare
    // token, or the name `/ID` — it is left alone.
    let mut inline_image_header: Option<usize> = None;
    let mut i = 0;
    while i < content.len() {
        match content[i] {
            b'(' => {
                let mut depth = 1u32;
                i += 1;
                while i < content.len() {
                    match content[i] {
                        b'\\' => {
                            masked[i] = b' ';
                            if i + 1 < content.len() {
                                masked[i + 1] = b' ';
                            }
                            i += 2;
                            continue;
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
                    masked[i] = b' ';
                    i += 1;
                }
            }
            b'<' if content.get(i + 1) == Some(&b'<') => i += 1,
            b'<' => {
                i += 1;
                while i < content.len() && content[i] != b'>' {
                    masked[i] = b' ';
                    i += 1;
                }
            }
            b'%' => {
                while i < content.len() && !matches!(content[i], b'\n' | b'\r') {
                    masked[i] = b' ';
                    i += 1;
                }
                continue;
            }
            b'B' if content.get(i + 1) == Some(&b'I')
                && after_token_break(content, i)
                && content.get(i + 2).is_none_or(|&b| ends_token(b)) =>
            {
                inline_image_header = Some(i + 2);
                i += 1;
            }
            b'I' if inline_image_header.is_some()
                && content.get(i + 1) == Some(&b'D')
                && after_token_break(content, i)
                && content.get(i + 2).is_none_or(|&b| is_pdf_whitespace(b)) =>
            {
                let header = &content[inline_image_header.take().unwrap_or(i)..i];
                let data = (i + 3).min(content.len());
                let end = inline_image_end(content, data).unwrap_or_else(|| {
                    data + inline_image_data_bound(header).min(content.len() - data)
                });
                masked[i..end].fill(b' ');
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    masked
}

/// Just past the `EI` that ends inline image data starting at `from`: the
/// first `EI` set off by whitespace on both sides, or failing that the
/// first `EI` that ends a token, which data written flush against its
/// `EI` leaves. `None` when there is neither.
fn inline_image_end(content: &[u8], from: usize) -> Option<usize> {
    let mut flush = None;
    let mut i = from;
    while i + 1 < content.len() {
        if content[i] == b'E'
            && content[i + 1] == b'I'
            && content.get(i + 2).is_none_or(|&b| ends_token(b))
        {
            if i > 0 && is_pdf_whitespace(content[i - 1]) {
                return Some(i + 2);
            }
            flush.get_or_insert(i + 2);
        }
        i += 1;
    }
    flush
}

/// How many bytes of inline image data to take when no `EI` ends it: the
/// data's own length when the header names no filter — width × height ×
/// components × bits per component, packed by row — or else a fixed
/// bound, so that a stream is never blanked to its end.
pub(super) fn inline_image_data_bound(header: &[u8]) -> usize {
    const UNKNOWN_LENGTH_BOUND: usize = 4096;
    let header = String::from_utf8_lossy(header);
    let tokens: Vec<&str> = header.split_ascii_whitespace().collect();
    let value_after = |keys: &[&str]| {
        tokens
            .iter()
            .position(|token| keys.contains(token))
            .and_then(|at| tokens.get(at + 1).copied())
    };
    let number_after = |keys: &[&str]| value_after(keys).and_then(|v| v.parse::<usize>().ok());
    if value_after(&["/F", "/Filter"]).is_some() {
        return UNKNOWN_LENGTH_BOUND;
    }
    let (Some(width), Some(height)) = (
        number_after(&["/W", "/Width"]),
        number_after(&["/H", "/Height"]),
    ) else {
        return UNKNOWN_LENGTH_BOUND;
    };
    let image_mask = matches!(value_after(&["/IM", "/ImageMask"]), Some("true"));
    let bits = if image_mask {
        1
    } else {
        number_after(&["/BPC", "/BitsPerComponent"]).unwrap_or(8)
    };
    let components = match value_after(&["/CS", "/ColorSpace"]) {
        Some("/RGB" | "/DeviceRGB" | "/CalRGB") => 3,
        Some("/CMYK" | "/DeviceCMYK") => 4,
        _ => 1,
    };
    let row_bytes = (width.saturating_mul(components).saturating_mul(bits)).div_ceil(8);
    row_bytes.saturating_mul(height)
}

/// The `N` numeric operands before the operator at `op_pos`, in stream
/// order; `None` when a token there is not a number or the lookback would
/// cross `floor`.
pub(super) fn numeric_operands_before<const N: usize>(
    content: &[u8],
    op_pos: usize,
    floor: usize,
) -> Option<[f64; N]> {
    let mut values = [0.0f64; N];
    let mut end = op_pos;
    for value in values.iter_mut().rev() {
        while end > floor && is_pdf_whitespace(content[end - 1]) {
            end -= 1;
        }
        let mut start = end;
        while start > floor && matches!(content[start - 1], b'0'..=b'9' | b'.' | b'-' | b'+') {
            start -= 1;
        }
        if start == end {
            return None;
        }
        *value = std::str::from_utf8(&content[start..end])
            .ok()?
            .parse()
            .ok()?;
        end = start;
    }
    Some(values)
}

/// The name operand (`/Name`, given without its slash, its escapes
/// decoded) before the operator at `op_pos`; `None` when the token there
/// is not a name or the lookback would cross `floor`.
pub(super) fn name_operand_before(content: &[u8], op_pos: usize, floor: usize) -> Option<Vec<u8>> {
    let mut end = op_pos;
    while end > floor && is_pdf_whitespace(content[end - 1]) {
        end -= 1;
    }
    let mut start = end;
    while start > floor && !is_pdf_name_delimiter(content[start - 1]) {
        start -= 1;
    }
    (start > floor && start < end && content[start - 1] == b'/')
        .then(|| decode_name_escapes(&content[start..end]))
}

/// The bytes a name written in a content stream stands for: each `#xx` —
/// a `#` and two hex digits — decoded to the byte it spells, as the
/// parser decodes the names that key a resource dictionary, so that
/// `/Im#30 Do` finds the `Im0` the resources bind. A `#` not followed by
/// two hex digits is kept as it is.
pub(super) fn decode_name_escapes(name: &[u8]) -> Vec<u8> {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let mut decoded = Vec::with_capacity(name.len());
    let mut i = 0;
    while i < name.len() {
        if name[i] == b'#' {
            if let (Some(&high), Some(&low)) = (name.get(i + 1), name.get(i + 2)) {
                if let (Some(high), Some(low)) = (hex(high), hex(low)) {
                    decoded.push(high << 4 | low);
                    i += 3;
                    continue;
                }
            }
        }
        decoded.push(name[i]);
        i += 1;
    }
    decoded
}

/// How many bytes of text the operand of the show operator at `op_pos`
/// holds: the bytes of a literal string, the digit pairs of a hex string,
/// or those of the strings among an array's elements — 0 when there is
/// nothing to show. `masked` is the stream with its strings blanked (see
/// [`mask_strings_comments_and_inline_images`]), in which a string's
/// delimiters pair up plainly; `content` is the stream itself. `() Tj`,
/// `<> Tj`, `[] TJ` and `[5 -8] TJ` show nothing. A literal's escape
/// sequences count by their bytes, near enough for the width the count
/// estimates.
pub(super) fn show_operand_text_bytes(
    masked: &[u8],
    content: &[u8],
    op_pos: usize,
    floor: usize,
) -> usize {
    fn opener_before(masked: &[u8], close: usize, floor: usize, opener: u8) -> Option<usize> {
        (floor..close).rev().find(|&at| masked[at] == opener)
    }
    fn closer_after(masked: &[u8], open: usize, end: usize, closer: u8) -> Option<usize> {
        (open + 1..end).find(|&at| masked[at] == closer)
    }
    fn literal_bytes(open: usize, close: usize) -> usize {
        close - open - 1
    }
    fn hex_bytes(content: &[u8], open: usize, close: usize) -> usize {
        content[open + 1..close]
            .iter()
            .filter(|byte| byte.is_ascii_hexdigit())
            .count()
            .div_ceil(2)
    }

    let mut close = op_pos;
    while close > floor && is_pdf_whitespace(masked[close - 1]) {
        close -= 1;
    }
    if close == floor {
        return 0;
    }
    let close = close - 1;
    match masked[close] {
        b')' => {
            opener_before(masked, close, floor, b'(').map_or(0, |open| literal_bytes(open, close))
        }
        b'>' => opener_before(masked, close, floor, b'<')
            .map_or(0, |open| hex_bytes(content, open, close)),
        b']' => {
            let Some(array_open) = opener_before(masked, close, floor, b'[') else {
                return 0;
            };
            let mut bytes = 0;
            let mut at = array_open + 1;
            while at < close {
                match masked[at] {
                    b'(' => {
                        let Some(end) = closer_after(masked, at, close, b')') else {
                            return 0;
                        };
                        bytes += literal_bytes(at, end);
                        at = end + 1;
                    }
                    b'<' => {
                        let Some(end) = closer_after(masked, at, close, b'>') else {
                            return 0;
                        };
                        bytes += hex_bytes(content, at, end);
                        at = end + 1;
                    }
                    _ => at += 1,
                }
            }
            bytes
        }
        _ => 0,
    }
}
