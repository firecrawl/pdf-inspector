//! Bounded stream decompression for detector scans.
//!
//! `lopdf::Stream::decompressed_content` materializes the full decoded buffer
//! before any caller can apply a limit. A few megabytes of Flate-compressed
//! zeros can therefore expand to gigabytes. These helpers stop inflate once
//! the decoded budget is reached.

use flate2::read::{DeflateDecoder, ZlibDecoder};
use lopdf::Stream;
use std::io::Read;

/// Maximum decoded bytes held for a single content stream during detection.
pub(crate) const MAX_DECOMPRESSED_STREAM_BYTES: usize = 32 * 1024 * 1024;

/// Decode `stream` for scanning, or return an empty buffer when the decoded
/// size would exceed `max_bytes`.
pub(crate) fn stream_content_for_scan(stream: &Stream) -> Vec<u8> {
    match decompressed_content_bounded(stream, MAX_DECOMPRESSED_STREAM_BYTES) {
        Some(data) => data,
        None => Vec::new(),
    }
}

/// Incremental decode with a hard output cap. `None` means the stream is
/// larger than `max_bytes` (or not safely decodable within that budget).
pub(crate) fn decompressed_content_bounded(stream: &Stream, max_bytes: usize) -> Option<Vec<u8>> {
    let filters = match stream.filters() {
        Ok(filters) => filters,
        Err(_) => {
            return take_if_within_budget(&stream.content, max_bytes);
        }
    };

    if filters.is_empty() {
        return take_if_within_budget(&stream.content, max_bytes);
    }

    // Plain Flate is the highly compressible case. Detector scans only need
    // the inflated operator bytes; skip PNG predictors here so inflate can
    // stop at the budget instead of materializing the full buffer first.
    if filters.len() == 1 && filters[0] == b"FlateDecode" {
        return inflate_flate_bounded(&stream.content, max_bytes);
    }

    if stream.content.len() > max_bytes {
        return None;
    }
    match stream.decompressed_content() {
        Ok(data) if data.len() <= max_bytes => Some(data),
        Ok(_) => None,
        Err(_) => take_if_within_budget(&stream.content, max_bytes),
    }
}

fn take_if_within_budget(bytes: &[u8], max_bytes: usize) -> Option<Vec<u8>> {
    if bytes.len() > max_bytes {
        None
    } else {
        Some(bytes.to_vec())
    }
}

fn inflate_flate_bounded(input: &[u8], max_bytes: usize) -> Option<Vec<u8>> {
    if input.is_empty() {
        return Some(Vec::new());
    }
    match read_bounded(ZlibDecoder::new(input), max_bytes) {
        Some(data) => Some(data),
        None if input.len() > 2 => read_bounded(DeflateDecoder::new(&input[2..]), max_bytes),
        None => None,
    }
}

fn read_bounded<R: Read>(mut decoder: R, max_bytes: usize) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match decoder.read(&mut buf) {
            Ok(0) => return Some(output),
            Ok(n) => {
                if output.len().saturating_add(n) > max_bytes {
                    return None;
                }
                output.extend_from_slice(&buf[..n]);
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use lopdf::dictionary;
    use std::io::Write;

    fn flate_stream(plain: &[u8]) -> Stream {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(plain).unwrap();
        let compressed = encoder.finish().unwrap();
        Stream::new(dictionary! { "Filter" => "FlateDecode" }, compressed)
    }

    #[test]
    fn small_flate_stream_round_trips() {
        let plain = b"BT /F1 12 Tf (Hello world) Tj ET";
        let stream = flate_stream(plain);
        assert_eq!(
            decompressed_content_bounded(&stream, MAX_DECOMPRESSED_STREAM_BYTES).as_deref(),
            Some(plain.as_slice())
        );
    }

    #[test]
    fn highly_compressible_flate_stops_at_budget() {
        let plain = vec![0u8; 256 * 1024];
        let stream = flate_stream(&plain);
        assert!(
            stream.content.len() < 8 * 1024,
            "fixture must stay compact on disk, got {} compressed bytes",
            stream.content.len()
        );
        assert!(decompressed_content_bounded(&stream, 16 * 1024).is_none());
        assert_eq!(
            decompressed_content_bounded(&stream, 256 * 1024).as_deref(),
            Some(plain.as_slice())
        );
    }

    #[test]
    fn uncompressed_over_budget_is_skipped() {
        let stream = Stream::new(dictionary! {}, vec![b'x'; 64]);
        assert!(decompressed_content_bounded(&stream, 32).is_none());
        assert_eq!(decompressed_content_bounded(&stream, 64).unwrap().len(), 64);
    }

    #[test]
    fn scan_helper_returns_empty_when_capped() {
        let stream = flate_stream(&vec![0u8; 64 * 1024]);
        // Production cap is far above 64 KiB, so this still decodes.
        assert_eq!(stream_content_for_scan(&stream).len(), 64 * 1024);
    }
}
