//! Same-length byte masks for the raw-key reload in `load_with_raw_file_key`.
//!
//! The reload must not see the trailer `/Encrypt` reference, and it must not
//! expand an object stream before that stream is decrypted. Both keys are
//! found by walking PDF tokens, so a match inside a string, a comment or a
//! stream payload is ignored, and escaped names (`/Obj#53tm`) or comments
//! between a key and its value (`/Type%note\n/ObjStm`) are still found.

/// Rename `/Encrypt` references in trailer and cross-reference stream
/// dictionaries, and `/Type /ObjStm` values in object stream dictionaries.
/// Each rename rewrites the first decoded character of the name to `X`
/// without changing its length, so xref offsets stay valid.
pub(crate) fn mask_encrypt_and_object_streams(buf: &mut [u8]) {
    for slash_at in find_masks(buf) {
        mask_first_name_char(buf, slash_at);
    }
}

fn mask_first_name_char(buf: &mut [u8], slash_at: usize) {
    if buf.get(slash_at + 1) == Some(&b'#') && slash_at + 3 < buf.len() {
        buf[slash_at + 2] = b'5';
        buf[slash_at + 3] = b'8';
    } else if slash_at + 1 < buf.len() {
        buf[slash_at + 1] = b'X';
    }
}

#[derive(Debug)]
enum Token {
    Name { start: usize, decoded: Vec<u8> },
    Int(i64),
    Keyword(Vec<u8>),
    Other,
    DictOpen,
    DictClose,
    ArrayOpen,
    ArrayClose,
}

struct Lexer<'a> {
    buf: &'a [u8],
    pos: usize,
}

fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn hex_value(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|digit| digit as u8)
}

impl<'a> Lexer<'a> {
    fn skip_whitespace_and_comments(&mut self) {
        while let Some(&byte) = self.buf.get(self.pos) {
            if is_whitespace(byte) {
                self.pos += 1;
            } else if byte == b'%' {
                while let Some(&byte) = self.buf.get(self.pos) {
                    if byte == b'\r' || byte == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn skip_literal_string(&mut self) {
        let mut depth = 0usize;
        while let Some(&byte) = self.buf.get(self.pos) {
            self.pos += 1;
            match byte {
                b'\\' => self.pos += 1,
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    fn next(&mut self) -> Option<Token> {
        self.skip_whitespace_and_comments();
        let start = self.pos;
        let byte = *self.buf.get(start)?;
        match byte {
            b'(' => {
                self.skip_literal_string();
                Some(Token::Other)
            }
            b'<' if self.buf.get(start + 1) == Some(&b'<') => {
                self.pos += 2;
                Some(Token::DictOpen)
            }
            b'<' => {
                while let Some(&byte) = self.buf.get(self.pos) {
                    self.pos += 1;
                    if byte == b'>' {
                        break;
                    }
                }
                Some(Token::Other)
            }
            b'>' if self.buf.get(start + 1) == Some(&b'>') => {
                self.pos += 2;
                Some(Token::DictClose)
            }
            b'[' => {
                self.pos += 1;
                Some(Token::ArrayOpen)
            }
            b']' => {
                self.pos += 1;
                Some(Token::ArrayClose)
            }
            b'/' => {
                self.pos += 1;
                let mut decoded = Vec::new();
                while let Some(&byte) = self.buf.get(self.pos) {
                    if is_whitespace(byte) || is_delimiter(byte) {
                        break;
                    }
                    let escaped = (byte == b'#')
                        .then(|| {
                            let high = hex_value(*self.buf.get(self.pos + 1)?)?;
                            let low = hex_value(*self.buf.get(self.pos + 2)?)?;
                            Some(high << 4 | low)
                        })
                        .flatten();
                    match escaped {
                        Some(value) => {
                            decoded.push(value);
                            self.pos += 3;
                        }
                        None => {
                            decoded.push(byte);
                            self.pos += 1;
                        }
                    }
                }
                Some(Token::Name { start, decoded })
            }
            _ if is_delimiter(byte) => {
                self.pos += 1;
                Some(Token::Other)
            }
            _ => {
                while let Some(&byte) = self.buf.get(self.pos) {
                    if is_whitespace(byte) || is_delimiter(byte) {
                        break;
                    }
                    self.pos += 1;
                }
                let text = &self.buf[start..self.pos];
                match std::str::from_utf8(text).ok().and_then(|t| t.parse().ok()) {
                    Some(value) if text.iter().all(u8::is_ascii_digit) => Some(Token::Int(value)),
                    _ => Some(Token::Keyword(text.to_vec())),
                }
            }
        }
    }

    /// Skip a stream payload. `length` is the dictionary's direct `/Length`;
    /// without one, or when it does not land on `endstream`, scan for the
    /// keyword followed by `endobj` instead.
    fn skip_stream_payload(&mut self, length: Option<usize>) {
        if self.buf.get(self.pos..self.pos + 2) == Some(b"\r\n") {
            self.pos += 2;
        } else if matches!(self.buf.get(self.pos), Some(b'\n' | b'\r')) {
            self.pos += 1;
        }
        if let Some(end) = length.and_then(|length| self.pos.checked_add(length)) {
            let mut after = end;
            while self.buf.get(after).is_some_and(|&byte| is_whitespace(byte)) {
                after += 1;
            }
            if self
                .buf
                .get(after..)
                .is_some_and(|rest| rest.starts_with(b"endstream"))
            {
                self.pos = after + b"endstream".len();
                return;
            }
        }
        self.pos = self.buf[self.pos.min(self.buf.len())..]
            .windows(b"endstream".len())
            .enumerate()
            .position(|(at, window)| {
                if window != b"endstream" {
                    return false;
                }
                let mut following = Lexer {
                    buf: self.buf,
                    pos: self.pos + at + b"endstream".len(),
                };
                following
                    .buf
                    .get(following.pos)
                    .is_some_and(|&byte| is_whitespace(byte) || byte == b'%')
                    && matches!(following.next(), Some(Token::Keyword(ref k)) if k == b"endobj")
            })
            .map_or(self.buf.len(), |at| self.pos + at + b"endstream".len());
    }

    /// `G R` after an integer, which makes the integer an object number.
    fn reference_follows(&mut self) -> bool {
        let saved = self.pos;
        let is_reference = matches!(self.next(), Some(Token::Int(_)))
            && matches!(self.next(), Some(Token::Keyword(ref k)) if k == b"R");
        if !is_reference {
            self.pos = saved;
        }
        is_reference
    }
}

enum Frame {
    Dict {
        key: Option<(usize, Vec<u8>)>,
        length: Option<usize>,
    },
    Array,
}

fn find_masks(buf: &[u8]) -> Vec<usize> {
    let mut lexer = Lexer { buf, pos: 0 };
    let mut stack: Vec<Frame> = Vec::new();
    let mut masks = Vec::new();
    let mut stream_length: Option<usize> = None;
    while let Some(token) = lexer.next() {
        if stack.is_empty() {
            match token {
                Token::Keyword(ref keyword) if keyword == b"stream" => {
                    lexer.skip_stream_payload(stream_length.take());
                    continue;
                }
                Token::DictOpen => {}
                _ => {
                    stream_length = None;
                    continue;
                }
            }
        }
        let outermost = stack.len() == 1;
        let pending_key = match stack.last_mut() {
            Some(Frame::Dict { key, .. }) => Some(key.take()),
            _ => None,
        };
        match (pending_key, token) {
            (_, Token::DictOpen) => stack.push(Frame::Dict {
                key: None,
                length: None,
            }),
            (_, Token::ArrayOpen) => stack.push(Frame::Array),
            (_, Token::DictClose) => {
                if let Some(Frame::Dict { length, .. }) = stack.pop() {
                    if stack.is_empty() {
                        stream_length = length;
                    }
                }
            }
            (_, Token::ArrayClose) => {
                stack.pop();
            }
            (Some(None), Token::Name { start, decoded }) => {
                if let Some(Frame::Dict { key, .. }) = stack.last_mut() {
                    *key = Some((start, decoded));
                }
            }
            (Some(Some((_, key))), Token::Name { start, decoded }) => {
                if outermost && key == b"Type" && decoded == b"ObjStm" {
                    masks.push(start);
                }
            }
            (Some(Some((key_at, key))), Token::Int(value)) => {
                let is_reference = lexer.reference_follows();
                if outermost && is_reference && key == b"Encrypt" {
                    masks.push(key_at);
                }
                if outermost && !is_reference && key == b"Length" {
                    if let Some(Frame::Dict { length, .. }) = stack.last_mut() {
                        *length = usize::try_from(value).ok();
                    }
                }
            }
            _ => {}
        }
    }
    masks
}

#[cfg(test)]
mod tests {
    use super::mask_encrypt_and_object_streams;

    fn masked(input: &[u8]) -> Vec<u8> {
        let mut buf = input.to_vec();
        mask_encrypt_and_object_streams(&mut buf);
        assert_eq!(buf.len(), input.len());
        buf
    }

    #[test]
    fn masks_plain_object_stream_and_trailer_encrypt() {
        let out =
            masked(b"5 0 obj\n<< /Type /ObjStm /N 1 >>\nendobj\ntrailer\n<< /Encrypt 7 0 R >>");
        assert_eq!(
            out,
            b"5 0 obj\n<< /Type /XbjStm /N 1 >>\nendobj\ntrailer\n<< /Xncrypt 7 0 R >>"
        );
    }

    #[test]
    fn masks_object_stream_type_after_a_comment() {
        let out = masked(b"5 0 obj\n<< /Type%note\n/ObjStm /N 1 >>\nendobj");
        assert_eq!(out, b"5 0 obj\n<< /Type%note\n/XbjStm /N 1 >>\nendobj");
    }

    #[test]
    fn masks_escaped_object_stream_names() {
        let out = masked(b"<< /Typ#65 /Obj#53tm >> << /Type /#4FbjStm >>");
        assert_eq!(out, b"<< /Typ#65 /Xbj#53tm >> << /Type /#58bjStm >>");
    }

    #[test]
    fn masks_encrypt_after_a_comment_and_escaped_encrypt_keys() {
        let out = masked(b"trailer\n<< /Encrypt%c\n 7 0 R /#45ncrypt 8 0 R >>");
        assert_eq!(out, b"trailer\n<< /Xncrypt%c\n 7 0 R /#58ncrypt 8 0 R >>");
    }

    #[test]
    fn ignores_names_inside_strings_comments_and_stream_payloads() {
        let input: &[u8] =
            b"1 0 obj\n<< /Title (see /Encrypt 7 0 R and /Type /ObjStm) /Hex <2F> >>\nendobj\n\
% /Encrypt 7 0 R\n\
2 0 obj\n<< /Length 29 >>\nstream\n/Encrypt 7 0 R /Type /ObjStm \nendstream\nendobj\n\
3 0 obj\n<< /Length 4 0 R >>\nstream\n/Encrypt 9 0 R\nendstream\nendobj\n\
trailer\n<< /Root 1 0 R /Encrypt 7 0 R >>";
        let out = masked(input);
        let expected =
            String::from_utf8_lossy(input).replace("/Root 1 0 R /Encrypt", "/Root 1 0 R /Xncrypt");
        assert_eq!(String::from_utf8_lossy(&out), expected);
    }

    #[test]
    fn ignores_nested_type_and_encrypt_entries() {
        let input: &[u8] =
            b"<< /Kids [ << /Type /ObjStm >> ] /Sub << /Encrypt 7 0 R >> /Encrypt 3 >>";
        assert_eq!(masked(input), input);
    }

    #[test]
    fn masks_trailer_encrypt_after_an_indirect_length_stream_with_false_endstream() {
        let input = b"2 0 obj\n<< /Length 20 0 R >>\nstream\nendstream <<\nendstream\nendobjSuffix\n/Encrypt 9 0 R\nendstream\nendobj\n20 0 obj\n50\nendobj\ntrailer\n<< /Encrypt 7 0 R >>";
        let expected = b"2 0 obj\n<< /Length 20 0 R >>\nstream\nendstream <<\nendstream\nendobjSuffix\n/Encrypt 9 0 R\nendstream\nendobj\n20 0 obj\n50\nendobj\ntrailer\n<< /Xncrypt 7 0 R >>";
        assert_eq!(masked(input), expected);
    }

    #[test]
    fn masks_trailer_encrypt_after_an_indirect_length_stream_with_endstream_comment() {
        let input = b"2 0 obj\n<< /Length 20 0 R >>\nstream\n/Encrypt 9 0 R\nendstream%comment\nendobj\n20 0 obj\n15\nendobj\ntrailer\n<< /Encrypt 7 0 R >>";
        let expected = b"2 0 obj\n<< /Length 20 0 R >>\nstream\n/Encrypt 9 0 R\nendstream%comment\nendobj\n20 0 obj\n15\nendobj\ntrailer\n<< /Xncrypt 7 0 R >>";
        assert_eq!(masked(input), expected);
    }

    #[test]
    fn masks_trailer_encrypt_after_an_incorrect_direct_length_stream() {
        let input = b"2 0 obj\n<< /Length 1 >>\nstream\nendstream <<\n/Encrypt 9 0 R\nendstream\r\n \tendobj\ntrailer\n<< /Encrypt 7 0 R >>";
        let expected = b"2 0 obj\n<< /Length 1 >>\nstream\nendstream <<\n/Encrypt 9 0 R\nendstream\r\n \tendobj\ntrailer\n<< /Xncrypt 7 0 R >>";
        assert_eq!(masked(input), expected);
    }

    #[test]
    fn uses_direct_stream_length_past_false_endstream_and_endobj() {
        let payload = b"endstream\nendobj\n<< /Encrypt 9 0 R";
        let input = format!(
            "2 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\ntrailer\n<< /Encrypt 7 0 R >>",
            payload.len(),
            String::from_utf8_lossy(payload)
        );
        let expected = input.replace("trailer\n<< /Encrypt", "trailer\n<< /Xncrypt");
        assert_eq!(masked(input.as_bytes()), expected.as_bytes());
    }
}
