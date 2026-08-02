//! Bounded decompression for PDF streams.
//!
//! `lopdf::Stream::decompressed_content` inflates FlateDecode streams fully
//! into memory with no output size limit (it calls `ZlibDecoder::read_to_end`
//! internally). A small file with an extreme compression ratio — a
//! "decompression bomb" — can therefore force an allocation orders of
//! magnitude larger than the file on disk before any of this crate's own
//! guards (e.g. `MAX_OPERATIONS` in `extractor::content_stream`) get a chance
//! to run, since those only bound the *parsed* content, not the raw bytes
//! produced by inflating it. A ~5 MB crafted PDF was measured driving this
//! crate's peak RSS past 2 GB via a single content stream.
//!
//! [`decompressed_content_capped`] re-implements just the plain-FlateDecode
//! path with a hard output cap, and falls back to `lopdf`'s own decoder for
//! everything the cap can't safely cover: uncompressed streams (can't
//! expand), non-Flate filters (LZWDecode/ASCII85Decode — not the observed
//! bomb vector), and Flate streams using a PNG/TIFF predictor (predictor
//! post-processing is image/xref-stream territory, not content streams, and
//! reimplementing it here would risk silently producing wrong bytes for the
//! sake of a case this crate does not exercise).

use lopdf::{Document, ObjectId, Stream};

/// Hard cap on the decompressed size of a single stream. Set well above
/// anything a legitimate content stream, CMap, or embedded font in this
/// crate's own fixtures needs, while keeping a decompression bomb's worst
/// case bounded to a fixed, small amount of RAM regardless of how the
/// on-disk PDF is sized.
pub(crate) const MAX_DECOMPRESSED_STREAM_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

/// Returns true if `stream` is FlateDecode-only with no PNG/TIFF predictor,
/// i.e. the case [`decompressed_content_capped`] can safely bound.
fn is_plain_flate(stream: &Stream) -> bool {
    let is_flate_only = matches!(stream.filters().as_deref(), Ok([f]) if *f == b"FlateDecode");
    if !is_flate_only {
        return false;
    }
    let has_predictor = stream
        .dict
        .get(b"DecodeParms")
        .and_then(lopdf::Object::as_dict)
        .ok()
        .and_then(|params| params.get(b"Predictor").ok())
        .and_then(|p| p.as_i64().ok())
        .map(|p| p != 1)
        .unwrap_or(false);
    !has_predictor
}

/// Like [`Stream::decompressed_content`], but caps FlateDecode output at
/// [`MAX_DECOMPRESSED_STREAM_BYTES`] instead of inflating an
/// attacker-controlled amount of data into memory. A stream that would
/// exceed the cap returns `Err` rather than a silently truncated buffer, so
/// callers don't work from chopped-off content.
pub(crate) fn decompressed_content_capped(stream: &Stream) -> Result<Vec<u8>, String> {
    if !is_plain_flate(stream) {
        return stream.decompressed_content().map_err(|e| e.to_string());
    }

    use flate2::read::ZlibDecoder;
    use std::io::Read;

    let decoder = ZlibDecoder::new(stream.content.as_slice());
    let mut output = Vec::new();
    let mut limited = decoder.take(MAX_DECOMPRESSED_STREAM_BYTES as u64 + 1);
    limited
        .read_to_end(&mut output)
        .map_err(|e| e.to_string())?;

    if output.len() > MAX_DECOMPRESSED_STREAM_BYTES {
        return Err(format!(
            "stream exceeds {MAX_DECOMPRESSED_STREAM_BYTES}-byte decompression cap"
        ));
    }
    Ok(output)
}

/// Bounded equivalent of `lopdf::Document::get_page_content`: concatenates a
/// page's content streams, decompressing each through
/// [`decompressed_content_capped`] instead of `lopdf`'s unbounded decoder.
/// This is the main extraction entry point (`extractor::content_stream`), so
/// it's the primary path a decompression bomb would otherwise take.
pub(crate) fn get_page_content_capped(doc: &Document, page_id: ObjectId) -> Vec<u8> {
    let mut content = Vec::new();
    for object_id in doc.get_page_contents(page_id) {
        if let Ok(lopdf::Object::Stream(stream)) = doc.get_object(object_id) {
            match decompressed_content_capped(stream) {
                Ok(data) => content.extend_from_slice(&data),
                Err(_) => content.extend_from_slice(&stream.content),
            }
        }
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::Dictionary;

    fn flate_stream(raw: &[u8]) -> Stream {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(raw).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut dict = Dictionary::new();
        dict.set("Filter", lopdf::Object::Name(b"FlateDecode".to_vec()));
        Stream::new(dict, compressed)
    }

    #[test]
    fn passes_through_small_flate_stream() {
        let raw = b"0 0 0 rg 0 0 1 1 re f".to_vec();
        let stream = flate_stream(&raw);
        let out = decompressed_content_capped(&stream).unwrap();
        assert_eq!(out, raw);
    }

    #[test]
    fn rejects_stream_exceeding_cap() {
        // Highly repetitive so it compresses tiny but inflates past the cap.
        let raw = vec![0u8; MAX_DECOMPRESSED_STREAM_BYTES + 1];
        let stream = flate_stream(&raw);
        let err = decompressed_content_capped(&stream).unwrap_err();
        assert!(err.contains("decompression cap"));
    }

    #[test]
    fn accepts_stream_exactly_at_cap() {
        let raw = vec![0u8; MAX_DECOMPRESSED_STREAM_BYTES];
        let stream = flate_stream(&raw);
        let out = decompressed_content_capped(&stream).unwrap();
        assert_eq!(out.len(), MAX_DECOMPRESSED_STREAM_BYTES);
    }

    #[test]
    fn falls_back_to_lopdf_for_uncompressed_stream() {
        let raw = b"uncompressed content".to_vec();
        let stream = Stream::new(Dictionary::new(), raw.clone());
        let out = decompressed_content_capped(&stream).unwrap();
        assert_eq!(out, raw);
    }
}
