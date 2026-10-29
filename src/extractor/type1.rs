//! The built-in encoding of an embedded Type 1 font program (`/FontFile`),
//! read from the program's cleartext part.

/// The encoding a Type 1 font program declares in the font dictionary of
/// its cleartext part: `StandardEncoding`, or an array naming the glyph of
/// each code it assigns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuiltinEncoding {
    Standard,
    /// The glyph name of each code the array assigns, by code; codes left
    /// at `.notdef` are not listed.
    Custom(Vec<(u8, String)>),
}

/// The built-in encoding of the Type 1 program `program` (the decompressed
/// bytes of a `/FontFile` stream): the `/Encoding` entry of the font
/// dictionary in its cleartext part, the text before `eexec`, at the level
/// of that dictionary rather than in one nested in it. The entry is
/// read in the two forms programs write it: `/Encoding StandardEncoding
/// def`, and `/Encoding 256 array` followed by an optional loop filling
/// every code with `.notdef` and by `dup <code> /<name> put` statements, up
/// to `readonly def`. `None` for anything else — no cleartext part, no
/// `/Encoding`, an entry in another form, a code outside a byte — so the
/// font reads as it would without one.
pub(crate) fn builtin_encoding(program: &[u8]) -> Option<BuiltinEncoding> {
    let mut tokens = Tokens {
        rest: cleartext(program)?,
    };
    // The font dictionary's own entry: at the level of the dictionary the
    // program begins, not in one nested in it (`/FontInfo … begin … end`).
    let mut depth = 0i32;
    loop {
        match tokens.next()? {
            Token::Word(b"begin") | Token::Open(b'<') => depth += 1,
            Token::Word(b"end") | Token::Close(b'>') => depth -= 1,
            Token::Name(b"Encoding") if depth == 1 => break,
            _ => {}
        }
    }
    match tokens.next()? {
        Token::Word(b"StandardEncoding") => {
            return ends_definition(&mut tokens).then_some(BuiltinEncoding::Standard);
        }
        Token::Word(size) if integer(size).is_some_and(|n| (1..=256).contains(&n)) => {}
        _ => return None,
    }
    if tokens.next()? != Token::Word(b"array") {
        return None;
    }
    let mut names: Vec<Option<String>> = vec![None; 256];
    loop {
        match tokens.next()? {
            Token::Word(b"dup") => {
                let Token::Word(code) = tokens.next()? else {
                    return None;
                };
                let code = u8::try_from(integer(code)?).ok()?;
                let Token::Name(name) = tokens.next()? else {
                    return None;
                };
                if tokens.next()? != Token::Word(b"put") {
                    return None;
                }
                names[usize::from(code)] =
                    (name != b".notdef").then(|| String::from_utf8_lossy(name).into_owned());
            }
            // `0 1 255 {1 index exch /.notdef put} for`: every code starts
            // as `.notdef`. A loop whose body puts anything else is not
            // read.
            Token::Word(first) if integer(first).is_some() => {
                for _ in 0..2 {
                    match tokens.next()? {
                        Token::Word(word) if integer(word).is_some() => {}
                        _ => return None,
                    }
                }
                if tokens.next()? != Token::Open(b'{') {
                    return None;
                }
                let (mut depth, mut notdef, mut put) = (1, false, false);
                while depth > 0 {
                    match tokens.next()? {
                        Token::Open(b'{') => depth += 1,
                        Token::Close(b'}') => depth -= 1,
                        Token::Name(b".notdef") => notdef = true,
                        Token::Name(_) => return None,
                        Token::Word(b"put") => put = true,
                        _ => {}
                    }
                }
                if !(notdef && put) || tokens.next()? != Token::Word(b"for") {
                    return None;
                }
            }
            Token::Word(b"readonly") => {}
            Token::Word(b"def") => break,
            _ => return None,
        }
    }
    Some(BuiltinEncoding::Custom(
        names
            .into_iter()
            .enumerate()
            .filter_map(|(code, name)| Some((code as u8, name?)))
            .collect(),
    ))
}

/// The cleartext part of a Type 1 program: the text before the `eexec`
/// operator, which starts the encrypted part. The operator is found as a
/// token, so a comment, a string or a name that spells it does not end the
/// part early. A PFB segment header some producers leave in front of it is
/// skipped.
fn cleartext(program: &[u8]) -> Option<&[u8]> {
    let program = match program {
        [0x80, 0x01, _, _, _, _, rest @ ..] => rest,
        _ => program,
    };
    let mut tokens = Tokens { rest: program };
    while let Some(token) = tokens.next() {
        if let Token::Word(word) = token {
            if word == b"eexec" {
                let end = program.len() - tokens.rest.len() - word.len();
                return Some(&program[..end]);
            }
        }
    }
    None
}

/// Whether the definition an entry's value opens ends where it should:
/// `def`, or `readonly def`.
fn ends_definition(tokens: &mut Tokens<'_>) -> bool {
    match tokens.next() {
        Some(Token::Word(b"def")) => true,
        Some(Token::Word(b"readonly")) => tokens.next() == Some(Token::Word(b"def")),
        _ => false,
    }
}

fn integer(word: &[u8]) -> Option<i64> {
    std::str::from_utf8(word).ok()?.parse().ok()
}

/// A PostScript token of a program's cleartext part.
#[derive(Debug, PartialEq, Eq)]
enum Token<'a> {
    /// A literal name, `/Encoding`, without its slash.
    Name(&'a [u8]),
    /// A number or an executable name: `256`, `dup`, `def`.
    Word(&'a [u8]),
    /// `{`, `[` or `<` (for `<<`).
    Open(u8),
    /// `}`, `]` or `>` (for `>>`).
    Close(u8),
    /// A string, literal or hexadecimal.
    Str,
}

struct Tokens<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        loop {
            let (&first, tail) = self.rest.split_first()?;
            match first {
                b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\0' => self.rest = tail,
                b'%' => {
                    let end = tail
                        .iter()
                        .position(|&b| b == b'\n' || b == b'\r')
                        .unwrap_or(tail.len());
                    self.rest = &tail[end..];
                }
                _ => break,
            }
        }
        let (&first, tail) = self.rest.split_first()?;
        let token = match first {
            b'(' => {
                // A literal string: balanced parentheses, backslash escapes.
                let mut depth = 1;
                let mut i = 0;
                while depth > 0 {
                    match tail.get(i)? {
                        b'\\' => i += 1,
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                self.rest = &tail[i..];
                Token::Str
            }
            b'<' if tail.first() == Some(&b'<') => {
                self.rest = &tail[1..];
                Token::Open(b'<')
            }
            b'>' if tail.first() == Some(&b'>') => {
                self.rest = &tail[1..];
                Token::Close(b'>')
            }
            b'<' => {
                let end = tail.iter().position(|&b| b == b'>')?;
                self.rest = &tail[end + 1..];
                Token::Str
            }
            b'{' | b'[' => {
                self.rest = tail;
                Token::Open(first)
            }
            b'}' | b']' => {
                self.rest = tail;
                Token::Close(first)
            }
            b'/' => {
                let end = word_end(tail);
                self.rest = &tail[end..];
                Token::Name(&tail[..end])
            }
            _ => {
                let end = word_end(self.rest).max(1);
                let word = &self.rest[..end];
                self.rest = &self.rest[end..];
                Token::Word(word)
            }
        };
        Some(token)
    }
}

/// The length of the regular characters at the start of `bytes`: up to
/// white space or a delimiter.
fn word_end(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .position(|&b| {
            matches!(
                b,
                b' ' | b'\t'
                    | b'\r'
                    | b'\n'
                    | b'\x0c'
                    | b'\0'
                    | b'('
                    | b')'
                    | b'<'
                    | b'>'
                    | b'['
                    | b']'
                    | b'{'
                    | b'}'
                    | b'/'
                    | b'%'
            )
        })
        .unwrap_or(bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Type 1 program whose cleartext part declares `encoding`, followed
    /// by `eexec` and bytes standing in for the encrypted part.
    fn program(encoding: &str) -> Vec<u8> {
        let mut program = format!(
            "%!PS-AdobeFont-1.0: TestFace 001.000\n\
             %%Notice: (a comment naming /Encoding StandardEncoding def)\n\
             11 dict begin\n\
             /FontInfo 7 dict dup begin\n\
             /Notice (A notice (with parentheses) and a % sign) readonly def\n\
             end readonly def\n\
             /FontName /TestFace def\n\
             /PaintType 0 def\n\
             /FontMatrix [0.001 0 0 0.001 0 0] readonly def\n\
             {encoding}\n\
             /FontBBox {{0 -250 1000 750}} readonly def\n\
             currentdict end\n\
             currentfile eexec\n"
        )
        .into_bytes();
        program.extend_from_slice(&[0xd9, 0xd6, 0x6f, 0x63, 0x3b, 0x84, 0x6a, 0x98]);
        program
    }

    fn custom(entries: &[(u8, &str)]) -> Option<BuiltinEncoding> {
        Some(BuiltinEncoding::Custom(
            entries
                .iter()
                .map(|&(code, name)| (code, name.to_string()))
                .collect(),
        ))
    }

    #[test]
    fn the_standard_encoding_token_is_read() {
        for entry in [
            "/Encoding StandardEncoding def",
            "/Encoding StandardEncoding readonly def",
        ] {
            assert_eq!(
                builtin_encoding(&program(entry)),
                Some(BuiltinEncoding::Standard),
                "{entry}"
            );
        }
    }

    #[test]
    fn an_encoding_array_names_the_glyph_of_each_code_it_assigns() {
        let entry = "/Encoding 256 array\n\
                     0 1 255 {1 index exch /.notdef put} for\n\
                     dup 11 /ff put\n\
                     dup 12 /fi put\n\
                     dup 34/quotedblright put\n\
                     dup 65 /A put\n\
                     dup 66 /.notdef put\n\
                     dup 123 /endash put\n\
                     dup 65 /Alpha put\n\
                     readonly def";
        // Codes in order, the last name put at a code standing, and a code
        // put back to `.notdef` left out.
        assert_eq!(
            builtin_encoding(&program(entry)),
            custom(&[
                (11, "ff"),
                (12, "fi"),
                (34, "quotedblright"),
                (65, "Alpha"),
                (123, "endash")
            ])
        );
        // Without the fill loop, and ended by a bare `def`.
        assert_eq!(
            builtin_encoding(&program("/Encoding 256 array dup 32 /space put def")),
            custom(&[(32, "space")])
        );
    }

    #[test]
    fn eexec_spelled_in_a_comment_a_string_or_a_name_does_not_end_the_cleartext() {
        let mut body = b"%!PS-AdobeFont-1.0: TestFace\n% the eexec part follows\n\
            /Notice (encrypted after eexec) readonly def\n/Marker /eexec def\n"
            .to_vec();
        body.extend_from_slice(&program("/Encoding 256 array dup 12 /fi put readonly def"));
        assert_eq!(builtin_encoding(&body), custom(&[(12, "fi")]));
    }

    #[test]
    fn an_encoding_key_in_a_nested_dictionary_is_not_the_fonts() {
        let body = b"%!PS-AdobeFont-1.0: TestFace\n11 dict begin\n\
            /FontInfo 7 dict dup begin\n/Encoding (a note, not the font's) readonly def\n\
            end readonly def\n/Encoding 256 array dup 12 /fi put readonly def\n\
            currentdict end\ncurrentfile eexec\n";
        assert_eq!(builtin_encoding(body), custom(&[(12, "fi")]));
    }

    #[test]
    fn a_fill_loop_that_puts_anything_but_notdef_reads_as_none() {
        for fill in [
            "0 1 255 {1 index exch /space put} for",
            "0 1 255 {pop} for",
            "0 1 255 {1 index exch /.notdef} for",
        ] {
            let entry = format!("/Encoding 256 array {fill} dup 12 /fi put readonly def");
            assert_eq!(builtin_encoding(&program(&entry)), None, "{fill}");
        }
    }

    #[test]
    fn a_pfb_segment_header_is_skipped() {
        let body = program("/Encoding 256 array dup 12 /fi put readonly def");
        let length = u32::try_from(body.len()).unwrap().to_le_bytes();
        let mut pfb = vec![0x80, 0x01];
        pfb.extend_from_slice(&length);
        pfb.extend_from_slice(&body);
        assert_eq!(builtin_encoding(&pfb), custom(&[(12, "fi")]));
    }

    #[test]
    fn an_entry_in_any_other_form_reads_as_none() {
        for entry in [
            // An encoding the program names that is not StandardEncoding.
            "/Encoding ISOLatin1Encoding def",
            // A literal array.
            "/Encoding [/.notdef /A /B] def",
            // A code outside a byte, a statement cut short, a stray token.
            "/Encoding 256 array dup 256 /A put readonly def",
            "/Encoding 256 array dup 65 /A readonly def",
            "/Encoding 256 array dup 65 /A put exch readonly def",
            // An array of an impossible size, or without its `array`.
            "/Encoding 0 array dup 65 /A put readonly def",
            "/Encoding 256 dup 65 /A put readonly def",
            // StandardEncoding not closed by `def`.
            "/Encoding StandardEncoding put",
            // No encoding entry at all.
            "",
        ] {
            assert_eq!(builtin_encoding(&program(entry)), None, "{entry}");
        }
        // No encoding entry ends before `eexec`, and no `eexec` at all.
        let mut unterminated = program("").to_vec();
        unterminated.extend_from_slice(b"/Encoding StandardEncoding def");
        assert_eq!(builtin_encoding(&unterminated), None);
        assert_eq!(
            builtin_encoding(b"/Encoding StandardEncoding def currentfile"),
            None
        );
        assert_eq!(builtin_encoding(b"/Encoding 256 array dup 65 /A put"), None);
    }
}
