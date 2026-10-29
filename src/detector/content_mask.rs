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

/// How many components a colour space named in an inline image's header
/// has, when the name is not a device space's — resolved by the scan in
/// the resources in force; `None` when it cannot say.
pub(super) type ColourSpaceComponents<'r> = &'r dyn Fn(&[u8]) -> Option<u32>;

/// `content` with everything that is not an operator or its operands
/// blanked to spaces, at the same offsets: the insides of literal strings
/// (nesting and escapes honoured), of hex strings and of comments, and
/// inline image data, from the `ID` of an inline image `BI` opened through
/// its `EI` — unfiltered data skipped by the length its header gives (see
/// [`inline_image_data_length`]), so that an `EI` among its bytes ends
/// nothing; filtered data, whose length is not known, to the first `EI`
/// set off by whitespace. The delimiters stay, so a string still closes an
/// operand; the strings' bytes are read from the original when a text
/// operator is found.
pub(super) fn mask_strings_comments_and_inline_images(
    content: &[u8],
    colour_space_components: ColourSpaceComponents<'_>,
) -> Vec<u8> {
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
                let end = match inline_image_data_length(header, colour_space_components) {
                    // The data's length is known: it is skipped whole,
                    // whatever bytes it holds, and the `EI` after it ends
                    // the image — or, should the length not lead to one,
                    // the first `EI` beyond the data does.
                    Some(length) => {
                        let after = data.saturating_add(length).min(content.len());
                        ei_at(content, after)
                            .or_else(|| inline_image_end(content, after))
                            .unwrap_or(after)
                    }
                    // Filtered data has no known length, nor has data under
                    // a header that cannot be read: the first `EI` set off
                    // by whitespace ends it, failing that the first flush
                    // `EI`, failing that a fixed bound, so that a stream is
                    // never blanked to its end.
                    None => inline_image_end(content, data).unwrap_or_else(|| {
                        data + UNKNOWN_INLINE_IMAGE_LENGTH_BOUND.min(content.len() - data)
                    }),
                };
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

/// Just past the `EI` at `at`, after any whitespace; `None` when the token
/// there is not `EI`.
fn ei_at(content: &[u8], at: usize) -> Option<usize> {
    let mut j = at;
    while j < content.len() && is_pdf_whitespace(content[j]) {
        j += 1;
    }
    (content[j..].starts_with(b"EI") && content.get(j + 2).is_none_or(|&b| ends_token(b)))
        .then_some(j + 2)
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

/// How many bytes of inline image data of unknown length to take when no
/// `EI` ends it.
const UNKNOWN_INLINE_IMAGE_LENGTH_BOUND: usize = 4096;

/// A token of an inline image's header: the entries between `BI` and `ID`.
enum HeaderToken<'h> {
    /// `/Name`, its escapes decoded.
    Name(Vec<u8>),
    Number(f64),
    /// A bare word: `true`, `false`, `null`.
    Word(&'h [u8]),
    ArrayOpen,
    ArrayClose,
    DictOpen,
    DictClose,
    /// A string, or a brace: a value that bears on nothing here.
    Other,
}

/// The header's tokens, split at whitespace and at delimiters — a name
/// runs into the next token without whitespace (`/W 1/H 1`), as names do;
/// a comment, which runs to the end of its line, is no token.
fn header_tokens(header: &[u8]) -> Vec<HeaderToken<'_>> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < header.len() {
        let byte = header[i];
        if is_pdf_whitespace(byte) {
            i += 1;
            continue;
        }
        match byte {
            b'/' => {
                let start = i + 1;
                let mut end = start;
                while end < header.len() && !is_pdf_name_delimiter(header[end]) {
                    end += 1;
                }
                tokens.push(HeaderToken::Name(decode_name_escapes(&header[start..end])));
                i = end;
            }
            b'[' => {
                tokens.push(HeaderToken::ArrayOpen);
                i += 1;
            }
            b']' => {
                tokens.push(HeaderToken::ArrayClose);
                i += 1;
            }
            b'<' if header.get(i + 1) == Some(&b'<') => {
                tokens.push(HeaderToken::DictOpen);
                i += 2;
            }
            b'>' if header.get(i + 1) == Some(&b'>') => {
                tokens.push(HeaderToken::DictClose);
                i += 2;
            }
            b'<' => {
                while i < header.len() && header[i] != b'>' {
                    i += 1;
                }
                tokens.push(HeaderToken::Other);
                i += 1;
            }
            b'(' => {
                let mut depth = 0u32;
                while i < header.len() {
                    match header[i] {
                        b'\\' => i += 1,
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                tokens.push(HeaderToken::Other);
                i += 1;
            }
            b'%' => {
                while i < header.len() && !matches!(header[i], b'\n' | b'\r') {
                    i += 1;
                }
            }
            b')' | b'>' | b'{' | b'}' => {
                tokens.push(HeaderToken::Other);
                i += 1;
            }
            _ => {
                let start = i;
                while i < header.len() && !is_pdf_name_delimiter(header[i]) {
                    i += 1;
                }
                let word = &header[start..i];
                tokens.push(
                    match std::str::from_utf8(word).ok().and_then(|w| w.parse().ok()) {
                        Some(number) => HeaderToken::Number(number),
                        None => HeaderToken::Word(word),
                    },
                );
            }
        }
    }
    tokens
}

/// How many components a device or CIE-based colour space has, by its
/// name as an inline image's header abbreviates it or a resource
/// dictionary spells it; `None` for any other.
pub(super) fn device_colour_space_components(name: &[u8]) -> Option<u32> {
    match name {
        b"G" | b"DeviceGray" | b"CalGray" | b"I" | b"Indexed" => Some(1),
        b"RGB" | b"DeviceRGB" | b"CalRGB" | b"Lab" => Some(3),
        b"CMYK" | b"DeviceCMYK" => Some(4),
        _ => None,
    }
}

/// The length in bytes of an inline image's data, from its header: the
/// `/L` (`/Length`) it states, or — for data with no `/F` (`/Filter`) —
/// width × components × bits per component bits per row, each row padded
/// to whole bytes, by the height; an image mask (`/IM true`) has one bit
/// per sample. The components come from `/CS` (`/ColorSpace`): a device
/// space by its name, an `/Indexed` array as one, any other name as the
/// resources in force say through `colour_space_components`. `None` when
/// the data is filtered and its length not stated, or the header cannot
/// be read — a width, height, bits or colour space missing or unknown —
/// and the data's end must be looked for instead. `/D` (`/Decode`) and
/// `/DP` bear on the samples' meaning, not on their count. An entry is
/// read from the header itself, never from a dictionary or array nested
/// in it: `/DP << /Columns 8 >>` names no width.
pub(super) fn inline_image_data_length(
    header: &[u8],
    colour_space_components: ColourSpaceComponents<'_>,
) -> Option<usize> {
    let tokens = header_tokens(header);
    // The index after the header's own entry named by one of `keys`: a name
    // inside a nested dictionary or array is a value, not an entry.
    let value_at = |keys: &[&[u8]]| {
        let mut depth = 0usize;
        for (at, token) in tokens.iter().enumerate() {
            match token {
                HeaderToken::ArrayOpen | HeaderToken::DictOpen => depth += 1,
                HeaderToken::ArrayClose | HeaderToken::DictClose => depth = depth.saturating_sub(1),
                HeaderToken::Name(name) if depth == 0 && keys.contains(&name.as_slice()) => {
                    return Some(at + 1);
                }
                _ => {}
            }
        }
        None
    };
    let integer_after = |keys: &[&[u8]]| match value_at(keys).and_then(|at| tokens.get(at)) {
        Some(HeaderToken::Number(number)) if number.fract() == 0.0 && *number >= 0.0 => {
            Some(*number as usize)
        }
        _ => None,
    };
    if let Some(length) = integer_after(&[b"L", b"Length"]) {
        return Some(length);
    }
    let filtered = match value_at(&[b"F", b"Filter"]).map(|at| (tokens.get(at), tokens.get(at + 1)))
    {
        Some((Some(HeaderToken::Name(_)), _)) => true,
        Some((Some(HeaderToken::ArrayOpen), next)) => {
            !matches!(next, Some(HeaderToken::ArrayClose))
        }
        _ => false,
    };
    if filtered {
        return None;
    }
    let width = integer_after(&[b"W", b"Width"])?;
    let height = integer_after(&[b"H", b"Height"])?;
    let image_mask = matches!(
        value_at(&[b"IM", b"ImageMask"]).and_then(|at| tokens.get(at)),
        Some(HeaderToken::Word(b"true"))
    );
    let (bits, components) = if image_mask {
        (1, 1)
    } else {
        let bits = integer_after(&[b"BPC", b"BitsPerComponent"])?;
        let at = value_at(&[b"CS", b"ColorSpace"])?;
        let components = match (tokens.get(at), tokens.get(at + 1)) {
            (Some(HeaderToken::Name(name)), _) => {
                device_colour_space_components(name).or_else(|| colour_space_components(name))?
            }
            (Some(HeaderToken::ArrayOpen), Some(HeaderToken::Name(family)))
                if matches!(family.as_slice(), b"I" | b"Indexed") =>
            {
                1
            }
            _ => return None,
        };
        (bits, components as usize)
    };
    let row_bytes = width
        .saturating_mul(components)
        .saturating_mul(bits)
        .div_ceil(8);
    Some(row_bytes.saturating_mul(height))
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
