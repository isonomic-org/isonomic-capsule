//! The recovery key, from the recovery kit's seed.
//!
//! An ID must commit to its recovery key when it is made: a key log lets
//! only a recovery key change the recovery commitments, so an ID made
//! without one could never gain one. The kit itself is not built yet
//! (isonomic-id issue 14), so the engine makes the kit's seed now, commits
//! the key it derives, and keeps the seed in the keychain until the kit
//! shows it to the person.
//!
//! The derivation is the kit's, fixed by the identity library's owner, so
//! it never varies: the 32 seed bytes are the Ed25519 private key (RFC 8032,
//! section 5.1.5). This is a local copy of the function requested for
//! `id-keys`; the test pins it to the RFC's own vector.

use id_core::{KeyAlg, PublicKey};
use ring::signature::{Ed25519KeyPair, KeyPair};

/// The length of a recovery seed.
pub const SEED_BYTES: usize = 32;

/// The recovery key's public half for `seed`.
pub fn recovery_public_key(seed: &[u8; SEED_BYTES]) -> PublicKey {
    // Any 32 bytes are a valid Ed25519 seed; this cannot fail.
    let pair = Ed25519KeyPair::from_seed_unchecked(seed).expect("a 32-byte seed");
    PublicKey::new(KeyAlg::Ed25519, pair.public_key().as_ref().to_vec())
        .expect("an Ed25519 key is 32 bytes")
}

/// A new seed from the system's generator.
pub fn new_seed() -> Result<[u8; SEED_BYTES], ring::error::Unspecified> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut seed = [0u8; SEED_BYTES];
    SystemRandom::new().fill(&mut seed)?;
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn the_seed_is_the_ed25519_private_key_as_rfc_8032_says() {
        // RFC 8032, section 7.1, test 1.
        let seed: [u8; 32] =
            unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
                .try_into()
                .unwrap();
        let public = recovery_public_key(&seed);
        assert_eq!(public.alg, KeyAlg::Ed25519);
        assert_eq!(
            public.bytes,
            unhex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
        );
    }

    #[test]
    fn every_seed_is_new() {
        assert_ne!(new_seed().unwrap(), new_seed().unwrap());
    }
}
