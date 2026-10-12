//! Recover the user password from an owner password.
//!
//! For standard security handlers of revision 4 and earlier, either password
//! opens the document, but the file encryption key is always derived from the
//! user password. lopdf 0.45.0 checks the owner password and then derives that
//! key from the owner password itself. Structure still parses (names, integers,
//! and references are not encrypted), and the decrypted streams are garbage,
//! which this crate reports as a confident scanned document. See
//! <https://github.com/firecrawl/pdf-inspector/issues/524>.

use crate::text_utils::pdf_doc_encoding_char;
use lopdf::Document;
use md5::{Digest, Md5};

const PAD_BYTES: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Password to give lopdf so Algorithm 2 derives the right file key.
///
/// `doc` is the structural load the caller already has. This reads `/O` from
/// that document and does not parse the file again.
///
/// `None` means `supplied` is already that password, or the recovered bytes
/// are not UTF-8. Those bytes are still the file key; the loader has to apply
/// them through [`user_password_bytes_for_file_key`] rather than this string.
pub(crate) fn user_password_for_file_key(doc: &Document, supplied: &str) -> Option<String> {
    let bytes = user_password_bytes_for_file_key(doc, supplied)?;
    String::from_utf8(bytes).ok()
}

/// User-password bytes Algorithm 2 hashes for the file key.
///
/// The string loader checks the password after PDFDocEncoding conversion, then
/// derives the file key from the string's UTF-8 bytes. Those match for ASCII.
/// A user password of 31 `a`s plus `é` is the single PDFDoc byte `0xE9`; the
/// UTF-8 form of that string is 33 bytes, and the 32-byte key prefix ends in
/// `0xC3`, so the text decrypts as garbage. `None` means the supplied string's
/// own bytes are already that key.
pub(crate) fn user_password_bytes_for_file_key(doc: &Document, supplied: &str) -> Option<Vec<u8>> {
    if supplied.is_empty() || !doc.is_encrypted() {
        return None;
    }
    let bytes = if doc.authenticate_user_password(supplied).is_ok() {
        pdf_doc_password_bytes(supplied)
    } else {
        let recovered = recover_padded_user_password(doc, supplied.as_bytes())?;
        let user = strip_password_padding(&recovered);
        if doc.authenticate_raw_user_password(&user).is_err() {
            return None;
        }
        user
    };
    if bytes.as_slice() == supplied.as_bytes() {
        return None;
    }
    Some(bytes)
}

/// PDFDocEncoding of `text`, truncated to the 32 bytes Algorithm 2 keeps.
/// Bytes lopdf's table leaves undefined are skipped, as its encoder does.
fn pdf_doc_password_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for ch in text.chars() {
        let Some(byte) = (0u8..=255)
            .find(|&byte| pdf_doc_byte_defined(byte) && pdf_doc_encoding_char(byte) == ch)
        else {
            continue;
        };
        out.push(byte);
        if out.len() == 32 {
            break;
        }
    }
    out
}

fn pdf_doc_byte_defined(byte: u8) -> bool {
    !matches!(byte, 0x00..=0x17 | 0x7F | 0x9F | 0xAD)
}

/// Algorithm 7 (revision 4 and earlier), stopping at the padded user password.
fn recover_padded_user_password(doc: &Document, owner_password: &[u8]) -> Option<Vec<u8>> {
    let dict = doc.get_encrypted().ok()?;
    let revision = dict.get(b"R").ok()?.as_i64().ok()?;
    if !(2..=4).contains(&revision) {
        return None;
    }
    let length_bits = dict
        .get(b"Length")
        .ok()
        .and_then(|object| object.as_i64().ok())
        .unwrap_or(40);
    let owner_value = dict.get(b"O").ok()?.as_str().ok()?.to_vec();
    if owner_value.len() != 32 {
        return None;
    }

    let len = owner_password.len().min(32);
    let mut hasher = Md5::new();
    hasher.update(&owner_password[..len]);
    hasher.update(&PAD_BYTES[..32 - len]);
    let mut hash = hasher.finalize();
    let n = if revision >= 3 {
        let n = length_bits / 8;
        if n <= 0 || n > 16 {
            return None;
        }
        for _ in 0..50 {
            hash = Md5::digest(hash);
        }
        n as usize
    } else if length_bits != 40 {
        return None;
    } else {
        5
    };

    let mut result = owner_value;
    if revision >= 3 {
        let mut key = vec![0u8; n];
        for i in (1..=19).rev() {
            for (in_byte, out_byte) in hash[..n].iter().zip(key.iter_mut()) {
                *out_byte = in_byte ^ (i as u8);
            }
            result = rc4(&key, &result);
        }
    }
    Some(rc4(&hash[..n], &result))
}

/// Drop the standard password padding appended to a short user password.
fn strip_password_padding(padded: &[u8]) -> Vec<u8> {
    for i in 0..=padded.len().min(32) {
        if padded.len() >= i && padded[i..] == PAD_BYTES[..32 - i] {
            return padded[..i].to_vec();
        }
    }
    padded.to_vec()
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s = [0u8; 256];
    for (i, byte) in s.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let mut i = 0u8;
    let mut j = 0u8;
    let mut out = Vec::with_capacity(data.len());
    for &byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
        out.push(byte ^ k);
    }
    out
}
