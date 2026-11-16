//! The recovery seed as words a person can write down.
//!
//! The standard 24-word encoding of 32 bytes: the 256 seed bits and the
//! first 8 bits of their SHA-256, cut into 24 groups of 11 bits, each the
//! index of a word in the standard 2048-word English list, bundled here
//! (`recovery_words.txt`) and pinned by its digest. The checksum catches a
//! mistyped or swapped word before any key is touched.

use sha2::{Digest, Sha256};

use crate::recovery::SEED_BYTES;

/// The bundled list, one word per line.
const LIST: &str = include_str!("recovery_words.txt");
/// The list's SHA-256, as published with the standard.
pub const LIST_SHA256: &str = "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda";
/// How many words a seed is.
pub const WORDS: usize = 24;

/// Why words are not a seed, in words a person can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WordsError {
    #[error("a recovery kit has 24 words; these are {0}")]
    Count(usize),
    /// The word itself is never repeated back: it may be the person's own.
    #[error("word {position} is not one of the recovery kit's words")]
    Unknown { position: usize },
    #[error("these words don't add up: one may be mistyped or out of order")]
    Checksum,
}

fn list() -> Vec<&'static str> {
    LIST.lines().collect()
}

/// The 24 words for `seed`.
pub fn encode(seed: &[u8; SEED_BYTES]) -> Vec<String> {
    let list = list();
    let check = Sha256::digest(seed)[0];
    let mut bits: Vec<bool> = Vec::with_capacity(264);
    for byte in seed.iter().chain(std::iter::once(&check)) {
        bits.extend((0..8).rev().map(|i| byte >> i & 1 == 1));
    }
    bits.chunks(11)
        .map(|group| {
            let index = group.iter().fold(0usize, |n, &b| n << 1 | usize::from(b));
            list[index].to_string()
        })
        .collect()
}

/// The seed `words` spell, if they are 24 of the list's words and their
/// checksum holds. Case and surrounding spaces are ignored.
pub fn decode(words: &[String]) -> Result<[u8; SEED_BYTES], WordsError> {
    if words.len() != WORDS {
        return Err(WordsError::Count(words.len()));
    }
    let list = list();
    let mut bits: Vec<bool> = Vec::with_capacity(264);
    for (i, word) in words.iter().enumerate() {
        let word = word.trim().to_lowercase();
        let index = list
            .binary_search(&word.as_str())
            .map_err(|_| WordsError::Unknown { position: i + 1 })?;
        bits.extend((0..11).rev().map(|b| index >> b & 1 == 1));
    }
    let byte = |chunk: &[bool]| chunk.iter().fold(0u8, |n, &b| n << 1 | u8::from(b));
    let bytes: Vec<u8> = bits.chunks(8).map(byte).collect();
    let seed: [u8; SEED_BYTES] = bytes[..SEED_BYTES].try_into().expect("32 bytes");
    if Sha256::digest(seed)[0] != bytes[SEED_BYTES] {
        return Err(WordsError::Checksum);
    }
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn the_bundled_list_is_the_standard_one() {
        let digest: String = Sha256::digest(LIST.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(digest, LIST_SHA256);
        let list = list();
        assert_eq!(list.len(), 2048);
        assert!(
            list.windows(2).all(|w| w[0] < w[1]),
            "sorted, for the search"
        );
    }

    #[test]
    fn the_standards_own_vectors_encode_and_decode() {
        // From the standard's published test vectors (256-bit entropy).
        let cases = [
            ([0u8; 32], "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art"),
            ([0xffu8; 32], "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo vote"),
        ];
        for (seed, text) in cases {
            assert_eq!(encode(&seed).join(" "), text);
            assert_eq!(decode(&words(text)).unwrap(), seed);
        }
        let seed: [u8; 32] = std::array::from_fn(|i| i as u8 * 7 + 3);
        assert_eq!(decode(&encode(&seed)).unwrap(), seed);
    }

    #[test]
    fn wrong_words_are_refused_plainly() {
        let mut w = encode(&[7u8; 32]);
        assert_eq!(decode(&w[..23]), Err(WordsError::Count(23)));
        w[3] = "notaword".into();
        assert!(matches!(
            decode(&w),
            Err(WordsError::Unknown { position: 4, .. })
        ));
        let mut w = encode(&[7u8; 32]);
        w.swap(0, 1);
        assert_eq!(decode(&w), Err(WordsError::Checksum));
        let mut w = encode(&[7u8; 32]);
        w[5] = format!("  {} ", w[5].to_uppercase());
        assert_eq!(
            decode(&w).unwrap(),
            [7u8; 32],
            "case and spaces are ignored"
        );
    }
}
