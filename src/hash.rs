//! SHA-256 digests as lowercase hexadecimal: the one text every stored or compared digest uses.
//!
//! The text is part of the contract. A release archive is verified against the digest its
//! `.sha256` asset publishes, an approved launcher is identified by its program's digest, and
//! identifiers and fingerprints are stored in the profile, so the output is always lowercase, with
//! no prefix and two digits per byte. `sha2` 0.11 digests no longer format with `{:x}`, which is
//! why the formatting lives here and nowhere else.
use sha2::{Digest, Sha256};
use std::{
    fmt::Write as _,
    io::{self, Read},
};

/// `bytes` as lowercase hexadecimal, two digits per byte (a digest, or a part of one).
pub fn hex(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// The SHA-256 of `data` as 64 lowercase hexadecimal digits.
pub fn sha256_hex(data: impl AsRef<[u8]>) -> String {
    hex(Sha256::digest(data.as_ref()))
}

/// The SHA-256 of everything `reader` yields, as 64 lowercase hexadecimal digits.
pub fn sha256_reader(mut reader: impl Read) -> io::Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(hex(hash.finalize())),
            Ok(read) => hash.update(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const A_100K: &str = "6d1cf22d7cc09b085dfc25ee1a1f3ae0265804c607bc2074ad253bcc82fd81ee";

    #[test]
    fn digests_are_lowercase_hex_without_a_prefix() {
        assert_eq!(sha256_hex(b""), EMPTY);
        assert_eq!(sha256_hex("abc"), ABC);
        assert_eq!(sha256_hex(String::from("abc")), ABC);
        assert_eq!(sha256_hex(vec![b'a'; 100_000]), A_100K);
    }

    #[test]
    fn a_reader_hashes_across_buffers() {
        assert_eq!(sha256_reader(io::empty()).unwrap(), EMPTY);
        assert_eq!(sha256_reader(&b"abc"[..]).unwrap(), ABC);
        assert_eq!(sha256_reader(&vec![b'a'; 100_000][..]).unwrap(), A_100K);
    }

    #[test]
    fn hex_keeps_leading_zeros_and_any_length() {
        assert_eq!(hex([0x00, 0x0a, 0xff]), "000aff");
        assert_eq!(hex(&Sha256::digest(b"abc")[..4]), &ABC[..8]);
        assert_eq!(hex([] as [u8; 0]), "");
    }
}
