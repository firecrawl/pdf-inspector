//! Bounded decompression for PDF streams.
//!
//! `lopdf::Stream::decompressed_content` inflates FlateDecode/LZWDecode
//! streams fully into memory with no output size limit. A small file with an
//! extreme compression ratio — a "decompression bomb" — can therefore force
//! an allocation orders of magnitude larger than the file on disk before any
//! of this crate's own guards (e.g. `MAX_OPERATIONS` in
//! `extractor::content_stream`) get a chance to run, since those only bound
//! *parsed* content, not the raw bytes produced by inflating it. A ~5 MB
//! crafted PDF was measured driving this crate's peak RSS past 2 GB via a
//! single content stream.
//!
//! [`decompressed_content_capped`] re-implements the two filters that can
//! expand data — FlateDecode and LZWDecode, including PNG/TIFF predictor
//! post-processing and, for Flate, `lopdf`'s corrupt-header raw-deflate
//! recovery — with a hard output cap enforced *during* decoding rather than
//! after. ASCII85Decode (which can only shrink data, ~4:5) is decoded with a
//! small local implementation so a multi-filter chain never has to fall back
//! to `lopdf`'s unbounded path; uncompressed streams pass through as-is,
//! since they can't expand. Unsupported filters return an error rather than
//! delegating to `lopdf`, since any future expanding filter added there would
//! otherwise silently bypass the cap.
//!
//! [`get_page_content_capped`] additionally bounds the *aggregate* size
//! across all of a page's content streams — many streams individually just
//! under the per-stream cap could otherwise still add up to an unbounded
//! total.

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

/// Hard cap on the decompressed size of a single stream. Set well above
/// anything a legitimate content stream, CMap, or embedded font in this
/// crate's own fixtures needs, while keeping a decompression bomb's worst
/// case bounded to a fixed, small amount of RAM regardless of how the
/// on-disk PDF is sized.
pub(crate) const MAX_DECOMPRESSED_STREAM_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

/// Hard cap on the combined decompressed size of a single page's content
/// streams. A page can have multiple `/Contents` streams (concatenated by
/// `get_page_content_capped`), so bounding each individually isn't enough —
/// enough streams just under [`MAX_DECOMPRESSED_STREAM_BYTES`] could still
/// add up to an unbounded total.
const MAX_PAGE_CONTENT_BYTES: usize = 256 * 1024 * 1024; // 256 MiB

/// Distinguishes "this stream genuinely failed to decode" (safe to fall back
/// to its raw bytes, matching `lopdf`'s own behavior) from "this stream
/// would decode fine but exceeds the cap" (falling back to raw — still
/// *compressed* — bytes would feed garbage into whatever parses the output,
/// so callers should skip the stream instead).
pub(crate) enum DecompressError {
    Failed(String),
    ExceedsCap,
}

impl DecompressError {
    fn failed(msg: impl Into<String>) -> Self {
        DecompressError::Failed(msg.into())
    }
}

/// Like [`Stream::decompressed_content`], but bounds every filter that can
/// expand data (FlateDecode, LZWDecode, and PNG/TIFF predictor
/// post-processing) at [`MAX_DECOMPRESSED_STREAM_BYTES`] instead of
/// inflating an attacker-controlled amount of data into memory.
pub(crate) fn decompressed_content_capped(stream: &Stream) -> Result<Vec<u8>, DecompressError> {
    let filters = match stream.filters() {
        Ok(f) => f,
        // No /Filter key means the stream is uncompressed; matches lopdf.
        Err(_) => return Ok(stream.content.clone()),
    };

    let params = stream
        .dict
        .get(b"DecodeParms")
        .and_then(Object::as_dict)
        .ok();

    let mut data = stream.content.clone();
    for filter in filters {
        data = match filter {
            b"FlateDecode" => bounded_zlib_with_predictor(&data, params)?,
            b"LZWDecode" => bounded_lzw_with_predictor(&data, params)?,
            b"ASCII85Decode" => decode_ascii85(&data)?,
            _ => {
                return Err(DecompressError::failed(
                    "unimplemented decompression algorithm",
                ))
            }
        };
        if data.len() > MAX_DECOMPRESSED_STREAM_BYTES {
            return Err(DecompressError::ExceedsCap);
        }
    }
    Ok(data)
}

/// Convenience wrapper for the common `unwrap_or_else(|_| stream.content.clone())`
/// pattern used throughout this crate. Only falls back to the stream's raw
/// bytes on a genuine decode [`DecompressError::Failed`] (matching `lopdf`'s
/// own recovery behavior); a stream that exceeds the cap returns an empty
/// buffer instead, since falling back to its raw — still compressed, for
/// Flate/LZW — bytes would feed binary garbage into whatever parses the
/// result (a CMap parser, `Content::decode`, a font parser) rather than the
/// clean "no data" a caller can already handle.
pub(crate) fn decompressed_or_raw(stream: &Stream) -> Vec<u8> {
    match decompressed_content_capped(stream) {
        Ok(data) => data,
        Err(DecompressError::Failed(msg)) => {
            log::debug!("stream decode failed ({msg}), falling back to raw bytes");
            stream.content.clone()
        }
        Err(DecompressError::ExceedsCap) => Vec::new(),
    }
}

/// Bounded equivalent of `lopdf::Document::get_page_content`: concatenates a
/// page's content streams, decompressing each through
/// [`decompressed_content_capped`] instead of `lopdf`'s unbounded decoder.
/// This is the main extraction entry point (`extractor::content_stream`), so
/// it's the primary path a decompression bomb would otherwise take.
///
/// Streams that exceed the cap are skipped (not substituted with their raw,
/// still-compressed bytes, which would otherwise get parsed as if they were
/// decoded PDF operators). Streams that genuinely fail to decode fall back
/// to their raw bytes, matching `lopdf`'s own recovery behavior for
/// already-uncompressed-but-mislabeled streams. The aggregate result is
/// capped at [`MAX_PAGE_CONTENT_BYTES`] regardless of how many streams
/// contribute to it.
pub(crate) fn get_page_content_capped(doc: &Document, page_id: ObjectId) -> Vec<u8> {
    let mut content = Vec::new();
    for object_id in doc.get_page_contents(page_id) {
        if let Ok(Object::Stream(stream)) = doc.get_object(object_id) {
            let data: std::borrow::Cow<[u8]> = match decompressed_content_capped(stream) {
                Ok(data) => data.into(),
                Err(DecompressError::ExceedsCap) => {
                    log::debug!(
                        "content stream {object_id:?} exceeds {MAX_DECOMPRESSED_STREAM_BYTES}-byte cap, skipping"
                    );
                    continue;
                }
                Err(DecompressError::Failed(msg)) => {
                    log::debug!(
                        "content stream {object_id:?} decode failed ({msg}), using raw bytes"
                    );
                    stream.content.as_slice().into()
                }
            };
            // Append only up to the remaining page budget instead of
            // extending in full and truncating afterward — extending first
            // lets a single near-cap-sized stream push the allocation up
            // to MAX_DECOMPRESSED_STREAM_BYTES past the intended cap, and
            // Vec::truncate doesn't release that over-allocated capacity.
            let remaining = MAX_PAGE_CONTENT_BYTES.saturating_sub(content.len());
            if remaining == 0 {
                log::debug!(
                    "page {page_id:?} content already at {MAX_PAGE_CONTENT_BYTES}-byte aggregate cap, stopping"
                );
                break;
            }
            let take = data.len().min(remaining);
            // Whether there's still room for the join separator after this
            // stream's data is appended in full (irrelevant when the
            // stream itself doesn't fit — that path breaks before reaching
            // the separator push below).
            let fits_with_separator = content.len() + take < MAX_PAGE_CONTENT_BYTES;
            // Reserve exactly what this iteration needs (data plus, when
            // applicable, the separator byte below) in one call — reserving
            // per-push instead would leave `push`'s own amortized-doubling
            // growth policy free to double the whole allocation just to fit
            // one more byte, which is what actually caused the retained
            // capacity to balloon past the cap here.
            content.reserve_exact(take + usize::from(fits_with_separator));
            content.extend_from_slice(&data[..take]);
            if take < data.len() {
                log::debug!(
                    "page {page_id:?} content exceeds {MAX_PAGE_CONTENT_BYTES}-byte aggregate cap, truncating"
                );
                break;
            }
            // Mirror lopdf::Document::get_page_content, which joins
            // multiple /Contents streams with a newline — content streams
            // can end/begin mid-token, and concatenating them bare can
            // merge adjacent operators into one invalid token. Only when
            // the stream fit in full and there's still budget left, so
            // this separator itself never pushes past the aggregate cap.
            if fits_with_separator {
                content.push(b'\n');
            }
        }
    }
    content
}

/// PNG/TIFF predictor post-processing, mirroring `lopdf`'s private
/// `Stream::decompress_predictor` via the public `lopdf::filters::png`
/// module. The predictor is a byte-for-byte reversible row transform (same
/// output length as input), so applying it after an already-capped inflate
/// can't reintroduce unbounded growth.
fn apply_predictor(data: Vec<u8>, params: Option<&Dictionary>) -> Result<Vec<u8>, DecompressError> {
    let Some(params) = params else {
        return Ok(data);
    };
    let predictor = params
        .get(b"Predictor")
        .and_then(Object::as_i64)
        .unwrap_or(1);
    if !(10..=15).contains(&predictor) {
        return Ok(data);
    }
    let pixels_per_row = params
        .get(b"Columns")
        .and_then(Object::as_i64)
        .unwrap_or(1)
        .max(1) as usize;
    let colors = params
        .get(b"Colors")
        .and_then(Object::as_i64)
        .unwrap_or(1)
        .max(1) as usize;
    let bits = params
        .get(b"BitsPerComponent")
        .and_then(Object::as_i64)
        .unwrap_or(8)
        .max(8) as usize;
    let bytes_per_pixel = colors * bits / 8;
    lopdf::filters::png::decode_frame(&data, bytes_per_pixel, pixels_per_row)
        .map_err(|e| DecompressError::failed(e.to_string()))
}

/// Bounded FlateDecode, including `lopdf`'s raw-deflate recovery for streams
/// with a corrupt zlib header/checksum (common in some encrypted PDFs), then
/// PNG/TIFF predictor post-processing.
fn bounded_zlib_with_predictor(
    input: &[u8],
    params: Option<&Dictionary>,
) -> Result<Vec<u8>, DecompressError> {
    use flate2::read::{DeflateDecoder, ZlibDecoder};
    use std::io::Read;

    let mut output = Vec::new();
    if !input.is_empty() {
        let decoder = ZlibDecoder::new(input);
        let mut limited = decoder.take(MAX_DECOMPRESSED_STREAM_BYTES as u64 + 1);
        if limited.read_to_end(&mut output).is_err() && output.is_empty() && input.len() > 2 {
            // Zlib decompression failed (e.g. corrupt adler32 checksum in
            // encrypted PDFs). Retry with raw deflate, skipping the 2-byte
            // zlib header and ignoring the checksum — mirrors lopdf exactly.
            output.clear();
            let decoder = DeflateDecoder::new(&input[2..]);
            let mut limited = decoder.take(MAX_DECOMPRESSED_STREAM_BYTES as u64 + 1);
            let _ = limited.read_to_end(&mut output);
        }
    }
    if output.len() > MAX_DECOMPRESSED_STREAM_BYTES {
        return Err(DecompressError::ExceedsCap);
    }
    apply_predictor(output, params)
}

/// Bounded LZWDecode via `weezl`'s incremental `decode_bytes` API (checking
/// cumulative output against the cap after every chunk, instead of
/// `Decoder::decode`'s unbounded `Vec`), then PNG/TIFF predictor
/// post-processing. Mirrors lopdf's exact decoder configuration
/// (`MIN_BITS`, `EarlyChange`/TIFF size switch, MSB bit order).
fn bounded_lzw_with_predictor(
    input: &[u8],
    params: Option<&Dictionary>,
) -> Result<Vec<u8>, DecompressError> {
    use weezl::{decode::Decoder, BitOrder, LzwStatus};
    const MIN_BITS: u8 = 9;

    let early_change = params
        .and_then(|p| p.get(b"EarlyChange").ok())
        .and_then(|p| Object::as_i64(p).ok())
        .map(|v| v != 0)
        .unwrap_or(true);
    let mut decoder = if early_change {
        Decoder::with_tiff_size_switch(BitOrder::Msb, MIN_BITS - 1)
    } else {
        Decoder::new(BitOrder::Msb, MIN_BITS - 1)
    };

    let mut output = Vec::new();
    let mut in_pos = 0usize;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let result = decoder.decode_bytes(&input[in_pos..], &mut buf);
        output.extend_from_slice(&buf[..result.consumed_out]);
        if output.len() > MAX_DECOMPRESSED_STREAM_BYTES {
            return Err(DecompressError::ExceedsCap);
        }
        in_pos += result.consumed_in;
        match result.status {
            Ok(LzwStatus::Ok) => continue,
            // `Done` (end marker) or `NoProgress` (input exhausted, nothing
            // more we can feed it) both mean "stop"; a genuine decode error
            // is logged and treated the same as lopdf's warn!-and-continue.
            Ok(LzwStatus::Done) | Ok(LzwStatus::NoProgress) => break,
            Err(e) => {
                log::warn!("LZW decode error: {e}");
                break;
            }
        }
    }
    apply_predictor(output, params)
}

/// Minimal Adobe/PDF-variant ASCII85 decoder. Reimplemented locally because
/// `lopdf::Stream::decode_ascii85` is private; kept in the same filter chain
/// as the bounded Flate/LZW paths above so a chain like
/// `[ASCII85Decode, FlateDecode]` never has to fall back to `lopdf`'s
/// unbounded `decompressed_content`. Decoding can only shrink data (5 ASCII
/// chars -> 4 bytes, or the `z` shorthand for 4 zero bytes), so no cap is
/// needed here — output is always <= input length.
/// Decodes one base-85 group into a big-endian u32, via checked arithmetic.
/// Valid ASCII85 groups always encode a value that fits (they come from
/// encoding a real `u32`), but 85^5 - 1 exceeds `u32::MAX`, so a malformed or
/// corrupted stream can present a 5-digit group with no valid decoding. That
/// must surface as an error rather than a wrapped/garbage value: silently
/// returning wrong bytes here means the caller unknowingly parses noise
/// instead of taking the `Failed` fallback path like every other malformed
/// stream in this module.
fn decode_ascii85_group(group: &[u8]) -> Result<u32, DecompressError> {
    group.iter().try_fold(0u32, |acc, &d| {
        acc.checked_mul(85)
            .and_then(|v| v.checked_add(d as u32))
            .ok_or_else(|| DecompressError::failed("ASCII85 group overflows u32"))
    })
}

fn decode_ascii85(input: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let mut out = Vec::with_capacity(input.len());
    let mut group = [0u8; 5];
    let mut group_len = 0usize;

    for &byte in input {
        if byte == b'~' {
            break;
        }
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'z' && group_len == 0 {
            // The 'z' shorthand expands one input byte to four output
            // bytes — ASCII85's only sub-linear expansion path, and the
            // only place in this decoder that can grow `out` faster than
            // one byte in roughly matches one byte out. A stream of
            // millions of 'z's would otherwise build an arbitrarily large
            // `out` before the caller's post-decode cap check ever runs.
            if out.len() > MAX_DECOMPRESSED_STREAM_BYTES.saturating_sub(4) {
                return Err(DecompressError::ExceedsCap);
            }
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&byte) {
            continue; // skip invalid characters rather than aborting the whole stream
        }
        group[group_len] = byte - b'!';
        group_len += 1;
        if group_len == 5 {
            let value = decode_ascii85_group(&group)?;
            out.extend_from_slice(&value.to_be_bytes());
            group_len = 0;
        }
    }

    if group_len > 1 {
        for slot in group.iter_mut().take(5).skip(group_len) {
            *slot = 84; // pad with 'u' - 33, matching the spec's padding value
        }
        let value = decode_ascii85_group(&group)?;
        let bytes = value.to_be_bytes();
        out.extend_from_slice(&bytes[..group_len - 1]);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flate_stream(raw: &[u8]) -> Stream {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(raw).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut dict = Dictionary::new();
        dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
        Stream::new(dict, compressed)
    }

    fn assert_ok(result: Result<Vec<u8>, DecompressError>) -> Vec<u8> {
        match result {
            Ok(data) => data,
            Err(DecompressError::Failed(msg)) => panic!("unexpected Failed: {msg}"),
            Err(DecompressError::ExceedsCap) => panic!("unexpected ExceedsCap"),
        }
    }

    #[test]
    fn passes_through_small_flate_stream() {
        let raw = b"0 0 0 rg 0 0 1 1 re f".to_vec();
        let stream = flate_stream(&raw);
        assert_eq!(assert_ok(decompressed_content_capped(&stream)), raw);
    }

    #[test]
    fn rejects_stream_exceeding_cap() {
        // Highly repetitive so it compresses tiny but inflates past the cap.
        let raw = vec![0u8; MAX_DECOMPRESSED_STREAM_BYTES + 1];
        let stream = flate_stream(&raw);
        assert!(matches!(
            decompressed_content_capped(&stream),
            Err(DecompressError::ExceedsCap)
        ));
    }

    #[test]
    fn accepts_stream_exactly_at_cap() {
        let raw = vec![0u8; MAX_DECOMPRESSED_STREAM_BYTES];
        let stream = flate_stream(&raw);
        assert_eq!(
            assert_ok(decompressed_content_capped(&stream)).len(),
            MAX_DECOMPRESSED_STREAM_BYTES
        );
    }

    #[test]
    fn falls_back_to_raw_for_uncompressed_stream() {
        let raw = b"uncompressed content".to_vec();
        let stream = Stream::new(Dictionary::new(), raw.clone());
        assert_eq!(assert_ok(decompressed_content_capped(&stream)), raw);
    }

    #[test]
    fn applies_png_predictor_after_bounded_inflate() {
        // 2 rows of one 3-component (RGB-like) pixel each: bytes_per_row =
        // bytes_per_pixel(3) * pixels_per_row(Columns=1) = 3, matching the
        // 3 data bytes after each row's filter-type byte below.
        // Row 0: filter=0 (None), pixel [10, 20, 30]
        // Row 1: filter=2 (Up), delta [1, 1, 1] -> decodes to [11, 21, 31]
        let raw = [0u8, 10, 20, 30, 2, 1, 1, 1];
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut encoder, &raw).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut params = Dictionary::new();
        params.set("Predictor", Object::Integer(12)); // PNG Up
        params.set("Columns", Object::Integer(1));
        params.set("Colors", Object::Integer(3));
        params.set("BitsPerComponent", Object::Integer(8));

        let mut dict = Dictionary::new();
        dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
        dict.set("DecodeParms", Object::Dictionary(params));
        let stream = Stream::new(dict, compressed);

        let out = assert_ok(decompressed_content_capped(&stream));
        assert_eq!(out, vec![10, 20, 30, 11, 21, 31]);
    }

    #[test]
    fn decodes_ascii85_alone() {
        // "Man " -> "9jqo^" is the canonical ASCII85 example (minus the `~>` terminator).
        let mut dict = Dictionary::new();
        dict.set("Filter", Object::Name(b"ASCII85Decode".to_vec()));
        let stream = Stream::new(dict, b"9jqo^~>".to_vec());
        assert_eq!(
            assert_ok(decompressed_content_capped(&stream)),
            b"Man ".to_vec()
        );
    }

    #[test]
    fn decodes_ascii85_z_shorthand() {
        let mut dict = Dictionary::new();
        dict.set("Filter", Object::Name(b"ASCII85Decode".to_vec()));
        let stream = Stream::new(dict, b"z~>".to_vec());
        assert_eq!(
            assert_ok(decompressed_content_capped(&stream)),
            vec![0, 0, 0, 0]
        );
    }

    #[test]
    fn ascii85_z_shorthand_respects_the_decompressed_size_cap() {
        // 'z' is ASCII85's only sub-linear-input expansion path: one input
        // byte becomes four output bytes. Enough of them must still hit
        // ExceedsCap during decoding rather than growing `out` past the
        // cap before the caller's post-decode length check ever runs.
        let mut raw = vec![b'z'; (MAX_DECOMPRESSED_STREAM_BYTES / 4) + 1];
        raw.extend_from_slice(b"~>");
        let mut dict = Dictionary::new();
        dict.set("Filter", Object::Name(b"ASCII85Decode".to_vec()));
        let stream = Stream::new(dict, raw);
        assert!(matches!(
            decompressed_content_capped(&stream),
            Err(DecompressError::ExceedsCap)
        ));
    }

    #[test]
    fn rejects_ascii85_group_overflowing_u32() {
        // "uuuuu" is the maximum possible 5-digit group (all digits = 84),
        // which decodes to 85^5 - 1 = 4_437_053_124 — past u32::MAX. A real
        // encoder never emits this; it only appears in malformed/corrupted
        // input, which must take the `Failed` path (not silently wrap into
        // an arbitrary decoded value).
        let mut dict = Dictionary::new();
        dict.set("Filter", Object::Name(b"ASCII85Decode".to_vec()));
        let stream = Stream::new(dict, b"uuuuu~>".to_vec());
        assert!(matches!(
            decompressed_content_capped(&stream),
            Err(DecompressError::Failed(_))
        ));
    }

    #[test]
    fn chained_ascii85_then_flate_never_falls_back_unbounded() {
        let raw = b"chained filter content".to_vec();
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut encoder, &raw).unwrap();
        let flate_compressed = encoder.finish().unwrap();

        // Encode the flate bytes as ASCII85 (reverse of decode_ascii85) so
        // the stream's raw content is ASCII85(Flate(raw)).
        let mut ascii85 = Vec::new();
        for chunk in flate_compressed.chunks(4) {
            let mut padded = [0u8; 4];
            padded[..chunk.len()].copy_from_slice(chunk);
            let value = u32::from_be_bytes(padded);
            if chunk.len() == 4 && value == 0 {
                ascii85.push(b'z');
                continue;
            }
            let mut digits = [0u8; 5];
            let mut v = value;
            for d in digits.iter_mut().rev() {
                *d = (v % 85) as u8 + b'!';
                v /= 85;
            }
            ascii85.extend_from_slice(&digits[..chunk.len() + 1]);
        }
        ascii85.extend_from_slice(b"~>");

        let mut dict = Dictionary::new();
        dict.set(
            "Filter",
            Object::Array(vec![
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        let stream = Stream::new(dict, ascii85);
        assert_eq!(assert_ok(decompressed_content_capped(&stream)), raw);
    }

    #[test]
    fn lzw_roundtrip_via_weezl_encoder() {
        let raw = b"LZW roundtrip test data LZW LZW LZW".to_vec();
        let mut encoder = weezl::encode::Encoder::new(weezl::BitOrder::Msb, 8);
        let compressed = encoder.encode(&raw).unwrap();

        let mut dict = Dictionary::new();
        dict.set("Filter", Object::Name(b"LZWDecode".to_vec()));
        let stream = Stream::new(dict, compressed);
        assert_eq!(assert_ok(decompressed_content_capped(&stream)), raw);
    }

    #[test]
    fn unimplemented_filter_fails_cleanly() {
        let mut dict = Dictionary::new();
        dict.set("Filter", Object::Name(b"CCITTFaxDecode".to_vec()));
        let stream = Stream::new(dict, vec![1, 2, 3]);
        assert!(matches!(
            decompressed_content_capped(&stream),
            Err(DecompressError::Failed(_))
        ));
    }

    #[test]
    fn get_page_content_capped_never_exceeds_aggregate_cap() {
        // Five streams, each individually well under
        // MAX_DECOMPRESSED_STREAM_BYTES (64 MiB) so none is rejected on its
        // own, but summing to well past MAX_PAGE_CONTENT_BYTES (256 MiB).
        // Appending each stream in full before truncating would let the
        // final append and the capacity growth it triggers retain memory
        // well past the cap even after truncate(); the fix appends only up
        // to the remaining budget, so both the length AND the retained
        // capacity must stay bounded.
        use lopdf::dictionary;
        const STREAM_RAW_BYTES: usize = 55 * 1024 * 1024;
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        let content_ids: Vec<Object> = (0..5)
            .map(|_| {
                let raw = vec![0u8; STREAM_RAW_BYTES];
                Object::Reference(doc.add_object(Object::Stream(flate_stream(&raw))))
            })
            .collect();

        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "Contents" => Object::Array(content_ids),
            }),
        );
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );

        let content = get_page_content_capped(&doc, page_id);
        assert!(
            content.len() <= MAX_PAGE_CONTENT_BYTES,
            "aggregate content ({} bytes) exceeded the {MAX_PAGE_CONTENT_BYTES}-byte cap",
            content.len()
        );
        // Extending in full before truncating would retain capacity for a
        // whole extra stream (55 MiB) beyond the cap; capacity should stay
        // close to the cap instead, not balloon toward the pre-truncation
        // 275 MiB five-stream total.
        assert!(
            content.capacity() < MAX_PAGE_CONTENT_BYTES + STREAM_RAW_BYTES,
            "retained capacity ({} bytes) suggests a stream was appended in \
             full before truncating, rather than only up to the remaining budget",
            content.capacity()
        );
    }
}
