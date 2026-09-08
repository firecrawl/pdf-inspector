//! Repair of classic cross-reference tables whose entries are 19 bytes long.
//!
//! Companion to the container repairs in `lib.rs` (`recover_startxref_pointer`,
//! `append_missing_eof_marker`): a candidate builder plus the minimal PDF
//! lexical helpers it needs to walk a trailer dictionary safely. Kept apart
//! from the extraction code so the cross-reference handling can evolve on
//! its own.

use std::collections::HashSet;

/// ISO 32000-1 s7.5.4 requires every classic cross-reference entry to be
/// exactly 20 bytes: `nnnnnnnnnn ggggg n` followed by a two-byte terminator
/// (SP CR, SP LF or CR LF). A long tail of generators — notably the
/// exporters behind several public-sector document archives — drop the
/// padding space and emit 19-byte entries ending in a bare LF or CR. mupdf, pdfium, pdf.js and qpdf all accept those; lopdf up to 0.44
/// matches none of the entries, leaves the section empty and fails the
/// whole load with "invalid file trailer" (fixed upstream in lopdf#564,
/// unreleased as of Sep 2026).
///
/// Instead of padding the entries in place — which would shift every byte
/// after the table and invalidate object offsets, `/Prev` links and
/// `startxref` — this walks the `startxref` → `/Prev` chain, re-emits each
/// classic table with conforming 20-byte entries *appended* at the end of
/// the buffer, re-links the copied trailers' `/Prev` to the appended copies,
/// and terminates with a fresh `startxref`/`%%EOF`. lopdf's `get_xref_start`
/// reads the *last* `%%EOF`, so the rebuilt chain transparently supersedes
/// the original without touching any existing offset.
///
/// Returns `None` (no candidate) when the chain has no 19-byte entries,
/// when any table in the chain is not a classic table this parser fully
/// understands (cross-reference streams, hybrid `/XRefStm` files,
/// mismatched section counts), or when the chain is implausibly long.
pub(crate) fn rebuild_short_xref_entries(buf: &[u8]) -> Option<Vec<u8>> {
    // A `startxref` whose in-range offset lands on garbage (a corrupted
    // pointer, a stray keyword in trailing junk) must not end the repair:
    // try the next candidate further back. A pointer that lands on a real
    // cross-reference structure we do not handle — an xref stream, a hybrid
    // file — is different: it names the document's current revision, and
    // rebuilding an older classic chain behind it would resurrect a stale
    // revision. Stop there and leave the file to lopdf.
    //
    // The table and entry budgets are shared across every candidate, so a
    // crafted tail cannot multiply the work by planting many pointers.
    let mut budget = ChainBudget::default();
    let mut parsed = None;
    for start in startxref_candidates(buf) {
        match parse_xref_chain(buf, start, &mut budget) {
            ChainOutcome::Parsed(chain) => {
                parsed = Some(chain);
                break;
            }
            ChainOutcome::Unsupported => return None,
            ChainOutcome::Garbage => continue,
        }
    }
    let (tables, total_entries, saw_short) = parsed?;
    // The first chain that parses is the document's answer: when it is
    // already conforming there is nothing to repair, and an older pointer
    // behind it (a previous incremental save) must not be resurrected.
    if !saw_short {
        return None;
    }

    let mut out = Vec::with_capacity(buf.len() + 64 + total_entries * 20);
    out.extend_from_slice(buf);
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    // Oldest table first, so each newer trailer's /Prev can point at the
    // already-appended copy of the table it used to reference.
    let mut prev_rebuilt: Option<usize> = None;
    for table in tables.iter().rev() {
        let pos = out.len();
        out.extend_from_slice(b"xref\n");
        for section in &table.sections {
            out.extend_from_slice(
                format!("{} {}\n", section.start, section.entries.len()).as_bytes(),
            );
            for entry in &section.entries {
                out.extend_from_slice(
                    format!(
                        "{:010} {:05} {} \n",
                        entry.offset,
                        entry.generation,
                        if entry.in_use { 'n' } else { 'f' }
                    )
                    .as_bytes(),
                );
            }
        }
        out.extend_from_slice(b"trailer\n");
        out.extend_from_slice(&rewrite_trailer_prev(&table.trailer, prev_rebuilt));
        out.push(b'\n');
        prev_rebuilt = Some(pos);
    }
    let newest = prev_rebuilt?;
    out.extend_from_slice(format!("startxref\n{newest}\n%%EOF\n").as_bytes());
    Some(out)
}

struct ClassicXrefEntry {
    offset: u64,
    generation: u32,
    in_use: bool,
}

struct ClassicXrefSection {
    start: u64,
    entries: Vec<ClassicXrefEntry>,
}

struct ClassicXrefTable {
    sections: Vec<ClassicXrefSection>,
    /// Raw bytes of the trailer dictionary, `<<` through `>>` inclusive.
    trailer: Vec<u8>,
    prev: Option<usize>,
    has_short_entries: bool,
}

/// Offsets named by `startxref` keywords, newest first, starting from the
/// one that belongs to the document's final `%%EOF` marker.
///
/// The marker is located anywhere in the buffer (not just a fixed tail
/// window, so a file with kilobytes of trailing data still resolves), and
/// keywords are yielded walking backwards from it in one pass. A keyword
/// must begin a line, optionally indented with spaces or tabs — that is what
/// the file format requires and what keeps a `startxref` inside a `%`
/// comment, a string or a longer token (`notstartxref`) from being taken for
/// a pointer. Keywords whose offset is missing, unparsable or out of range
/// are skipped. Whether an offset actually leads to a parseable table is the
/// caller's decision, so a bogus pointer never hides an earlier valid one.
fn startxref_candidates(buf: &[u8]) -> impl Iterator<Item = usize> + '_ {
    const KEYWORD: &[u8] = b"startxref";
    const EOF: &[u8] = b"%%EOF";
    let search_end = buf
        .windows(EOF.len())
        .rposition(|w| w == EOF)
        .unwrap_or(buf.len());
    // One backwards walk over the buffer: `at` only ever decreases, so the
    // cost is linear however many bogus keywords a hostile file plants.
    let mut at = search_end.saturating_sub(KEYWORD.len()) + 1;
    std::iter::from_fn(move || {
        while at > 0 {
            at -= 1;
            if !buf[at..].starts_with(KEYWORD) {
                continue;
            }
            let after = at + KEYWORD.len();
            if !buf.get(after).is_some_and(u8::is_ascii_whitespace) || !starts_a_line(buf, at) {
                continue;
            }
            let pos = skip_ws(buf, after);
            let digits_end = skip_digits(buf, pos);
            if digits_end == pos || digits_end > search_end {
                continue;
            }
            let Some(offset) = std::str::from_utf8(&buf[pos..digits_end])
                .ok()
                .and_then(|d| d.parse::<usize>().ok())
            else {
                continue;
            };
            if offset < buf.len() {
                return Some(offset);
            }
        }
        None
    })
}
/// Work budget shared by every chain a single repair attempts.
struct ChainBudget {
    tables: usize,
    entries: usize,
}

impl Default for ChainBudget {
    fn default() -> Self {
        Self {
            tables: 64,
            entries: 5_000_000,
        }
    }
}

enum ChainOutcome {
    /// Tables newest first, total entry count, whether any has 19-byte entries.
    Parsed((Vec<ClassicXrefTable>, usize, bool)),
    /// The pointer names a cross-reference structure this repair does not
    /// handle (xref stream, hybrid file, a classic table it cannot fully
    /// parse, or a chain over budget). The document's current revision lives
    /// there, so no older pointer may be tried.
    Unsupported,
    /// The pointer lands on bytes that are no cross-reference structure at
    /// all; an earlier pointer may still be valid.
    Garbage,
}

/// Walks the `startxref` → `/Prev` chain from `start`.
fn parse_xref_chain(buf: &[u8], start: usize, budget: &mut ChainBudget) -> ChainOutcome {
    let mut tables: Vec<ClassicXrefTable> = Vec::new();
    let mut seen = HashSet::new();
    let mut next = Some(start);
    let mut saw_short = false;
    let mut total_entries = 0usize;
    while let Some(off) = next {
        if budget.tables == 0 || !seen.insert(off) {
            return ChainOutcome::Unsupported;
        }
        let table = match parse_classic_xref_table(buf, off) {
            TableOutcome::Table(table) => table,
            TableOutcome::Unsupported => return ChainOutcome::Unsupported,
            // Garbage behind a /Prev is a broken chain, not a broken pointer.
            TableOutcome::Garbage if tables.is_empty() => return ChainOutcome::Garbage,
            TableOutcome::Garbage => return ChainOutcome::Unsupported,
        };
        // Only real tables draw on the budget: a pointer that lands on
        // garbage is rejected in a few bytes and must not be able to starve
        // the valid pointer behind it.
        budget.tables -= 1;
        let entries: usize = table.sections.iter().map(|s| s.entries.len()).sum();
        if entries > budget.entries {
            return ChainOutcome::Unsupported;
        }
        budget.entries -= entries;
        total_entries += entries;
        saw_short |= table.has_short_entries;
        next = table.prev;
        tables.push(table);
    }
    ChainOutcome::Parsed((tables, total_entries, saw_short))
}

/// Whether the token at `pos` is the first thing on its line, allowing
/// leading spaces and tabs.
fn starts_a_line(buf: &[u8], pos: usize) -> bool {
    let mut i = pos;
    while i > 0 && matches!(buf[i - 1], b' ' | b'\t') {
        i -= 1;
    }
    i == 0 || matches!(buf[i - 1], b'\n' | b'\r')
}

fn skip_ws(buf: &[u8], mut pos: usize) -> usize {
    while buf.get(pos).is_some_and(u8::is_ascii_whitespace) {
        pos += 1;
    }
    pos
}

fn skip_digits(buf: &[u8], mut pos: usize) -> usize {
    while buf.get(pos).is_some_and(u8::is_ascii_digit) {
        pos += 1;
    }
    pos
}

fn parse_uint<T: std::str::FromStr>(buf: &[u8], pos: usize) -> Option<(T, usize)> {
    let end = skip_digits(buf, pos);
    if end == pos {
        return None;
    }
    let value = std::str::from_utf8(&buf[pos..end]).ok()?.parse().ok()?;
    Some((value, end))
}

enum TableOutcome {
    Table(ClassicXrefTable),
    Unsupported,
    Garbage,
}

/// Parses one classic cross-reference table starting at `off` (the `xref`
/// keyword), including its trailer dictionary. Accepts both conforming
/// 20-byte entries and the 19-byte bare-LF/CR variants, and reports whether
/// any of the latter were seen.
///
/// When the table cannot be fully parsed the outcome decides whether an
/// earlier `startxref` may be tried, and the rule is deliberately
/// conservative. Anything that could be a cross-reference structure — the
/// `xref` keyword, or an indirect object header `N G obj` (where a
/// cross-reference stream would live) — is `Unsupported`, even when it is
/// malformed or turns out to be an ordinary object: a pointer that lands
/// there may well name the document's current revision, and rebuilding an
/// older chain behind it would silently load stale contents. Only bytes
/// that are neither are `Garbage`. The cost of being wrong in this
/// direction is a file that stays unrepaired, exactly as it is today; the
/// cost in the other direction is wrong output.
fn parse_classic_xref_table(buf: &[u8], off: usize) -> TableOutcome {
    let Some(table) = parse_classic_xref_table_inner(buf, off) else {
        let pos = skip_ws(buf, off);
        return if buf[pos..].starts_with(b"xref") || is_indirect_object_header(buf, pos) {
            TableOutcome::Unsupported
        } else {
            TableOutcome::Garbage
        };
    };
    TableOutcome::Table(table)
}

/// Whether `pos` starts an indirect object header, `N G obj`.
fn is_indirect_object_header(buf: &[u8], pos: usize) -> bool {
    parse_uint::<u64>(buf, pos)
        .and_then(|(_, p)| parse_uint::<u32>(buf, skip_ws(buf, p)))
        .is_some_and(|(_, p)| buf[skip_ws(buf, p)..].starts_with(b"obj"))
}

fn parse_classic_xref_table_inner(buf: &[u8], off: usize) -> Option<ClassicXrefTable> {
    let mut pos = skip_ws(buf, off);
    if !buf[pos..].starts_with(b"xref") {
        return None;
    }
    pos = skip_ws(buf, pos + b"xref".len());

    let mut sections = Vec::new();
    let mut has_short_entries = false;
    loop {
        if buf[pos..].starts_with(b"trailer") {
            break;
        }
        let (start, after_start): (u64, usize) = parse_uint(buf, pos)?;
        let sep = skip_ws(buf, after_start);
        if sep == after_start {
            return None;
        }
        let (count, after_count): (usize, usize) = parse_uint(buf, sep)?;
        pos = skip_ws(buf, after_count);

        let mut entries = Vec::with_capacity(count.min(1 << 16));
        for _ in 0..count {
            // nnnnnnnnnn ggggg [nf]
            let (offset, p) = parse_uint::<u64>(buf, pos)?;
            if p - pos != 10 || buf.get(p) != Some(&b' ') {
                return None;
            }
            let (generation, p2) = parse_uint::<u32>(buf, p + 1)?;
            if p2 - (p + 1) != 5 || buf.get(p2) != Some(&b' ') {
                return None;
            }
            let in_use = match buf.get(p2 + 1) {
                Some(b'n') => true,
                Some(b'f') => false,
                _ => return None,
            };
            let term = p2 + 2;
            // Terminator: conforming " \r", " \n", "\r\n" (or the 21-byte
            // " \r\n" some writers emit), else the short bare "\r" / "\n".
            let rest = &buf[term..];
            let term_len = if rest.starts_with(b" \r\n") {
                3
            } else if rest.starts_with(b" \r")
                || rest.starts_with(b" \n")
                || rest.starts_with(b"\r\n")
            {
                2
            } else if rest.starts_with(b"\n") || rest.starts_with(b"\r") {
                has_short_entries = true;
                1
            } else {
                return None;
            };
            pos = term + term_len;
            entries.push(ClassicXrefEntry {
                offset,
                generation,
                in_use,
            });
        }
        sections.push(ClassicXrefSection { start, entries });
        pos = skip_ws(buf, pos);
        if sections.len() > 4096 {
            return None;
        }
    }

    pos = skip_ws(buf, pos + b"trailer".len());
    let (dict_start, dict_end) = pdf_dictionary_span(buf, pos)?;
    let trailer = buf[dict_start..dict_end].to_vec();
    if find_dict_key(&trailer, b"/XRefStm").is_some() {
        // Hybrid-reference file: the real entries live in a stream we do
        // not rebuild; leave it to lopdf as-is.
        return None;
    }
    let prev = match find_dict_key(&trailer, b"/Prev") {
        Some(value_pos) => {
            let (prev, _): (usize, usize) = parse_uint(&trailer, value_pos)?;
            if prev >= buf.len() {
                return None;
            }
            Some(prev)
        }
        None => None,
    };

    Some(ClassicXrefTable {
        sections,
        trailer,
        prev,
        has_short_entries,
    })
}

/// Span `[start, end)` of the dictionary beginning at `pos` (which must be
/// `<<`), honouring nested dictionaries and skipping literal strings, hex
/// strings and `%` comments so a `>>` inside any of those does not end it
/// early.
fn pdf_dictionary_span(buf: &[u8], pos: usize) -> Option<(usize, usize)> {
    if !buf[pos..].starts_with(b"<<") {
        return None;
    }
    let mut depth = 0usize;
    let mut i = pos;
    while i < buf.len() {
        match buf[i] {
            b'<' if buf[i..].starts_with(b"<<") => {
                depth += 1;
                i += 2;
            }
            b'>' if buf[i..].starts_with(b">>") => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return Some((pos, i));
                }
            }
            b'<' => i = skip_hex_string(buf, i)?,
            b'(' => i = skip_literal_string(buf, i)?,
            b'%' => i = skip_comment(buf, i),
            _ => i += 1,
        }
    }
    None
}

/// End of the literal string opening at `pos` (`(`), honouring `\` escapes
/// and balanced nested parentheses.
fn skip_literal_string(buf: &[u8], pos: usize) -> Option<usize> {
    let mut nest = 0usize;
    let mut i = pos;
    loop {
        i += 1;
        match buf.get(i)? {
            b'\\' => i += 1,
            b'(' => nest += 1,
            b')' if nest == 0 => return Some(i + 1),
            b')' => nest -= 1,
            _ => {}
        }
    }
}

/// End of the hex string opening at `pos` (`<`): the byte after its `>`.
fn skip_hex_string(buf: &[u8], pos: usize) -> Option<usize> {
    let mut i = pos + 1;
    while *buf.get(i)? != b'>' {
        i += 1;
    }
    Some(i + 1)
}

/// End of the `%` comment starting at `pos`: the byte after its line ending
/// (or the end of the buffer).
fn skip_comment(buf: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while let Some(&b) = buf.get(i) {
        i += 1;
        if b == b'\r' || b == b'\n' {
            break;
        }
    }
    i
}

fn skip_ws_and_comments(buf: &[u8], mut pos: usize) -> usize {
    loop {
        pos = skip_ws(buf, pos);
        if buf.get(pos) == Some(&b'%') {
            pos = skip_comment(buf, pos);
        } else {
            return pos;
        }
    }
}

fn is_pdf_delimiter_or_ws(b: u8) -> bool {
    b.is_ascii_whitespace()
        || matches!(
            b,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

/// End of the regular token (name body, number, keyword) starting at `pos`.
fn pdf_token_end(buf: &[u8], mut pos: usize) -> usize {
    while buf.get(pos).is_some_and(|&b| !is_pdf_delimiter_or_ws(b)) {
        pos += 1;
    }
    pos
}

/// End of the whole PDF object starting at `pos`: a nested dictionary or
/// array, a literal or hex string, a name, or a bare token — where a bare
/// integer followed by another integer and `R` is consumed as one indirect
/// reference. `None` for malformed input or nesting deeper than a sane
/// trailer ever needs.
fn skip_pdf_object(buf: &[u8], pos: usize, depth: usize) -> Option<usize> {
    const MAX_DEPTH: usize = 32;
    if depth > MAX_DEPTH {
        return None;
    }
    match *buf.get(pos)? {
        b'<' if buf[pos..].starts_with(b"<<") => pdf_dictionary_span(buf, pos).map(|(_, end)| end),
        b'<' => skip_hex_string(buf, pos),
        b'(' => skip_literal_string(buf, pos),
        b'/' => Some(pdf_token_end(buf, pos + 1)),
        b'[' => {
            let mut i = pos + 1;
            loop {
                i = skip_ws_and_comments(buf, i);
                if *buf.get(i)? == b']' {
                    return Some(i + 1);
                }
                i = skip_pdf_object(buf, i, depth + 1)?;
            }
        }
        b')' | b'>' | b']' | b'{' | b'}' | b'%' => None,
        _ => {
            let end = pdf_token_end(buf, pos);
            if end == pos {
                return None;
            }
            if buf[pos..end].iter().all(u8::is_ascii_digit) {
                // `n g R` indirect reference: consume all three tokens.
                let gen_start = skip_ws_and_comments(buf, end);
                let gen_end = pdf_token_end(buf, gen_start);
                if gen_end > gen_start && buf[gen_start..gen_end].iter().all(u8::is_ascii_digit) {
                    let r = skip_ws_and_comments(buf, gen_end);
                    if buf.get(r) == Some(&b'R')
                        && buf.get(r + 1).is_none_or(|&b| is_pdf_delimiter_or_ws(b))
                    {
                        return Some(r + 1);
                    }
                }
            }
            Some(end)
        }
    }
}

/// Position of the value following the top-level key `key` (including its
/// leading `/`) in a dictionary's raw bytes, `<<` through `>>`. Walks the
/// dictionary as key/value pairs and skips each value whole — nested
/// dictionaries and arrays, literal and hex strings, comments, `n g R`
/// references — so the same bytes inside a value are never mistaken for the
/// key. `None` when the key is absent or the dictionary is malformed.
fn find_dict_key(dict: &[u8], key: &[u8]) -> Option<usize> {
    if !dict.starts_with(b"<<") {
        return None;
    }
    let mut pos = 2;
    loop {
        pos = skip_ws_and_comments(dict, pos);
        if dict.get(pos)? != &b'/' {
            // `>>` (end of dictionary) or anything that is not a key.
            return None;
        }
        let name_end = pdf_token_end(dict, pos + 1);
        let value_pos = skip_ws_and_comments(dict, name_end);
        if &dict[pos..name_end] == key {
            return Some(value_pos);
        }
        pos = skip_pdf_object(dict, value_pos, 0)?;
    }
}

/// Copy of `trailer` with its `/Prev` value replaced by `new_prev`; when the
/// trailer has no `/Prev`, or `new_prev` is `None`, the bytes are unchanged.
fn rewrite_trailer_prev(trailer: &[u8], new_prev: Option<usize>) -> Vec<u8> {
    let (Some(value_pos), Some(new_prev)) = (find_dict_key(trailer, b"/Prev"), new_prev) else {
        return trailer.to_vec();
    };
    let value_end = skip_digits(trailer, value_pos);
    let mut out = Vec::with_capacity(trailer.len() + 8);
    out.extend_from_slice(&trailer[..value_pos]);
    out.extend_from_slice(new_prev.to_string().as_bytes());
    out.extend_from_slice(&trailer[value_end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load_document_from_mem;

    /// Minimal one-page PDF whose classic xref entries use `term` as their
    /// terminator; `prev_chain` adds an incremental update whose trailer
    /// `/Prev`-links to the first table.
    fn synthetic_pdf_with_xref_terminator(term: &str, prev_chain: bool) -> Vec<u8> {
        let header = "%PDF-1.7\n";
        let obj1 = "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
        let obj2 = "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";
        let obj3 = "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n";
        let o1 = header.len();
        let o2 = o1 + obj1.len();
        let o3 = o2 + obj2.len();
        let body = format!("{header}{obj1}{obj2}{obj3}");
        let xref1 = body.len();
        let mut doc = format!(
            "{body}xref\n0 4\n0000000000 65535 f{term}{o1:010} 00000 n{term}{o2:010} 00000 n{term}\
             {o3:010} 00000 n{term}trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref1}\n%%EOF\n"
        );
        if prev_chain {
            let o3b = doc.len();
            let obj3b =
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] >>\nendobj\n";
            doc.push_str(obj3b);
            let xref2 = doc.len();
            doc.push_str(&format!(
                "xref\n3 1\n{o3b:010} 00000 n{term}trailer\n<< /Size 4 /Root 1 0 R /Prev {xref1} >>\n\
                 startxref\n{xref2}\n%%EOF\n"
            ));
        }
        doc.into_bytes()
    }

    #[test]
    fn find_dict_key_only_matches_top_level_keys() {
        // `/Prev` and `/XRefStm` appear inside a literal string, a hex
        // string, a nested dictionary, an array and a comment before the real
        // top-level `/Prev`; none of those may be taken for the key.
        let dict = b"<< /Size 4 /Info (see /Prev 99) /Extra << /Prev 5 /XRefStm 6 >>\n\
                     /ID [<2f5072657620> (/XRefStm 8)] % /XRefStm 7 >>\n\
                     /Root 1 0 R /Prev 42 >>";
        let value = find_dict_key(dict, b"/Prev").expect("top-level /Prev");
        assert!(
            dict[value..].starts_with(b"42"),
            "{}",
            String::from_utf8_lossy(&dict[value..])
        );
        assert_eq!(find_dict_key(dict, b"/XRefStm"), None);
        assert_eq!(
            find_dict_key(dict, b"/Pre"),
            None,
            "key match must be whole-token"
        );
        let root = find_dict_key(dict, b"/Root").expect("/Root");
        assert!(dict[root..].starts_with(b"1 0 R"));
    }

    #[test]
    fn pdf_dictionary_span_skips_comments_and_strings() {
        let dict = b"<< /A (x >> y) /B <2f3e3e> % trailing >> comment\n/C << /D 1 >> >>";
        let (start, end) = pdf_dictionary_span(dict, 0).expect("balanced dictionary");
        assert_eq!((start, end), (0, dict.len()));
        assert_eq!(pdf_dictionary_span(b"<< /A (unterminated", 0), None);
    }

    #[test]
    fn rewrite_trailer_prev_leaves_prev_inside_values_alone() {
        let trailer = b"<< /Info (/Prev 1) /Prev 10 /Root 1 0 R >>";
        let out = rewrite_trailer_prev(trailer, Some(777));
        assert_eq!(out, b"<< /Info (/Prev 1) /Prev 777 /Root 1 0 R >>".to_vec());
    }

    #[test]
    fn startxref_candidates_anchor_on_the_final_eof() {
        // A stray `startxref 999` after the final %%EOF (trailing junk, a
        // viewer note) must not win over the keyword paired with the marker.
        let buf = b"%PDF-1.4\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 >>\nstartxref\n9\n%%EOF\n% note: startxref 999\n";
        assert_eq!(startxref_candidates(buf).next(), Some(9));
        // The keyword must be a standalone token.
        assert_eq!(
            startxref_candidates(b"%PDF-1.4\nnotstartxref\n9\n%%EOF\n").next(),
            None
        );
        assert_eq!(
            startxref_candidates(b"%PDF-1.4\nstartxrefs\n9\n%%EOF\n").next(),
            None
        );
        // A keyword followed by no digits before %%EOF is skipped, not misread.
        assert_eq!(
            startxref_candidates(b"%PDF-1.4\nstartxref\n7\nstartxref\n%%EOF\n").next(),
            Some(7)
        );
        // Without any %%EOF the last standalone keyword still resolves.
        assert_eq!(
            startxref_candidates(b"%PDF-1.4\nstartxref\n5\n").next(),
            Some(5)
        );
        // Leading spaces or tabs on the keyword's line are fine.
        assert_eq!(
            startxref_candidates(b"%PDF-1.4\n  \tstartxref\n8\n%%EOF\n").next(),
            Some(8)
        );
        // A `startxref` inside a comment line is not a pointer.
        assert_eq!(
            startxref_candidates(b"%PDF-1.4\nstartxref\n3\n% startxref\n999\n%%EOF\n").next(),
            Some(3)
        );
        // An unparsable or out-of-range offset is skipped, not fatal.
        assert_eq!(
            startxref_candidates(
                b"%PDF-1.4\nstartxref\n4\nstartxref\n99999999999999999999999\n%%EOF\n"
            )
            .next(),
            Some(4)
        );
        assert_eq!(
            startxref_candidates(b"%PDF-1.4\nstartxref\n4\nstartxref\n500000\n%%EOF\n").next(),
            Some(4)
        );
        // The final %%EOF is found beyond any fixed tail window.
        let mut long_tail = b"%PDF-1.4\nstartxref\n6\n%%EOF\n".to_vec();
        long_tail.extend(std::iter::repeat_n(b'x', 5000));
        long_tail.extend_from_slice(b"\nstartxref\n999\n");
        assert_eq!(startxref_candidates(&long_tail).next(), Some(6));
    }

    #[test]
    fn startxref_candidates_yield_earlier_pointers_after_a_bogus_one() {
        let buf = b"%PDF-1.4\nstartxref\n3\nstartxref\n12\n%%EOF\n";
        assert_eq!(startxref_candidates(buf).collect::<Vec<_>>(), vec![12, 3]);
    }

    #[test]
    fn short_xref_entries_load_when_the_last_startxref_points_at_garbage() {
        // An incremental save gone wrong: a trailing `startxref` whose
        // in-range offset lands in the middle of an object. The repair must
        // fall back to the earlier, valid pointer rather than give up.
        let mut doc = synthetic_pdf_with_xref_terminator("\n", false);
        doc.extend_from_slice(b"startxref\n20\n%%EOF\n");
        let repaired = rebuild_short_xref_entries(&doc).expect("earlier pointer should be used");
        assert!(repaired.starts_with(&doc));
        let (_, pages) = load_document_from_mem(&doc).expect("loads through the repair path");
        assert_eq!(pages, 1);
    }

    #[test]
    fn rebuild_stops_when_the_newest_pointer_is_an_xref_stream() {
        // Newest revision uses a cross-reference stream this repair does not
        // handle; the older classic chain behind it must NOT be rebuilt, or
        // lopdf would load the stale revision.
        let mut doc = synthetic_pdf_with_xref_terminator("\n", false);
        let stream_obj = doc.len();
        doc.extend_from_slice(
            b"5 0 obj\n<< /Type /XRef /Size 6 /W [1 2 1] /Root 1 0 R >>\nstream\nendstream\nendobj\n",
        );
        doc.extend_from_slice(format!("startxref\n{stream_obj}\n%%EOF\n").as_bytes());
        assert!(rebuild_short_xref_entries(&doc).is_none());
    }

    #[test]
    fn rebuild_stops_when_the_newest_table_is_a_hybrid_file() {
        let mut doc = synthetic_pdf_with_xref_terminator("\n", false);
        let table = doc.len();
        doc.extend_from_slice(
            b"xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 6 /Root 1 0 R /XRefStm 99 >>\n",
        );
        doc.extend_from_slice(format!("startxref\n{table}\n%%EOF\n").as_bytes());
        assert!(rebuild_short_xref_entries(&doc).is_none());
    }

    #[test]
    fn bogus_pointers_do_not_starve_the_valid_one() {
        // 100 in-range `startxref` pointers into unrelated bytes, then the
        // real short-entry table further back: the repair must still happen.
        let mut doc = synthetic_pdf_with_xref_terminator("\n", false);
        for _ in 0..100 {
            doc.extend_from_slice(b"startxref\n20\n");
        }
        doc.extend_from_slice(b"%%EOF\n");
        assert!(rebuild_short_xref_entries(&doc).is_some());
        let (_, pages) = load_document_from_mem(&doc).expect("loads through the repair path");
        assert_eq!(pages, 1);
    }

    #[test]
    fn a_pointer_at_an_object_header_stops_the_search() {
        // Conservative by design: a pointer landing on `N G obj` might be a
        // malformed xref stream naming the current revision, so the older
        // short-entry chain behind it is left alone (see
        // `parse_classic_xref_table`).
        let mut doc = synthetic_pdf_with_xref_terminator("\n", false);
        let catalog = doc.windows(7).position(|w| w == b"1 0 obj").unwrap();
        doc.extend_from_slice(format!("startxref\n{catalog}\n%%EOF\n").as_bytes());
        assert!(rebuild_short_xref_entries(&doc).is_none());
    }

    #[test]
    fn a_pointer_at_the_xref_keyword_stops_the_search() {
        // Same conservative rule for the `xref` keyword, malformed or not.
        let mut doc = synthetic_pdf_with_xref_terminator("\n", false);
        let junk = doc.len();
        doc.extend_from_slice(b"xref\nnot a table\n");
        doc.extend_from_slice(format!("startxref\n{junk}\n%%EOF\n").as_bytes());
        assert!(rebuild_short_xref_entries(&doc).is_none());
    }

    #[test]
    fn a_pointer_into_plain_junk_falls_back_to_the_earlier_pointer() {
        let mut doc = synthetic_pdf_with_xref_terminator("\n", false);
        let junk = doc.len();
        doc.extend_from_slice(b"% viewer note\n");
        doc.extend_from_slice(format!("startxref\n{junk}\n%%EOF\n").as_bytes());
        assert!(rebuild_short_xref_entries(&doc).is_some());
        let (_, pages) = load_document_from_mem(&doc).expect("loads through the repair path");
        assert_eq!(pages, 1);
    }

    #[test]
    fn rebuild_short_xref_entries_is_a_noop_for_conforming_tables() {
        for term in [" \n", " \r", "\r\n"] {
            let doc = synthetic_pdf_with_xref_terminator(term, false);
            assert!(
                rebuild_short_xref_entries(&doc).is_none(),
                "conforming terminator {term:?} must not produce a repair candidate"
            );
        }
    }

    #[test]
    fn rebuild_short_xref_entries_appends_a_conforming_chain() {
        for term in ["\n", "\r"] {
            let doc = synthetic_pdf_with_xref_terminator(term, false);
            let repaired =
                rebuild_short_xref_entries(&doc).expect("19-byte entries should be rebuilt");
            assert!(
                repaired.starts_with(&doc),
                "original bytes must be untouched"
            );
            let tail = &repaired[doc.len()..];
            assert!(
                tail.starts_with(b"xref\n0 4\n0000000000 65535 f \n"),
                "{}",
                String::from_utf8_lossy(tail)
            );
            assert!(tail.ends_with(b"%%EOF\n"));
        }
    }

    #[test]
    fn short_xref_entries_load_through_the_repair_path() {
        // The document-level contract: lopdf alone rejects the 19-byte form,
        // the repair candidate makes it load with the right page count. The
        // 20-byte control proves the loader is otherwise identical.
        for (name, term, prev_chain) in [
            ("bare LF", "\n", false),
            ("bare CR", "\r", false),
            ("bare LF with /Prev chain", "\n", true),
            ("conforming control", " \n", false),
        ] {
            let doc = synthetic_pdf_with_xref_terminator(term, prev_chain);
            let (_, pages) = load_document_from_mem(&doc).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(pages, 1, "{name}");
        }
    }

    #[test]
    fn rebuild_short_xref_entries_relinks_prev_to_the_rebuilt_copy() {
        let doc = synthetic_pdf_with_xref_terminator("\n", true);
        let repaired = rebuild_short_xref_entries(&doc).expect("chain should be rebuilt");
        let tail = String::from_utf8_lossy(&repaired[doc.len()..]);
        // Oldest table first; the newer trailer's /Prev must point into the tail.
        let first_xref = doc.len() + tail.find("xref\n").unwrap();
        let prev_pos = tail.find("/Prev ").unwrap() + "/Prev ".len();
        let prev: usize = tail[prev_pos..]
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(prev, first_xref, "{tail}");
        assert!(
            rebuild_short_xref_entries(&repaired).is_none(),
            "rebuilt output is conforming"
        );
    }
}
