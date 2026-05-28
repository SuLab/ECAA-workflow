//! SHA-256 hashing helpers. Consolidates 19+ scattered
//! `Sha256::new() + update + finalize` sites across the workspace.
//!
//! Existing call sites can migrate onto this module incrementally.

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Hex-encoded SHA-256 of `bytes`. 64-character lowercase string.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// First `n` hex characters of [`sha256_hex`]. Used by callers that just
/// need a short content-addressable suffix (e.g. derived-image tags).
///
/// `n` larger than 64 yields the full 64-character digest (no panic).
pub fn sha256_short(bytes: &[u8], n: usize) -> String {
    let full = sha256_hex(bytes);
    full.chars().take(n).collect()
}

/// Serialize `v` to canonical JSON bytes and return the SHA-256 hex digest.
/// Useful for stamping structured values (settings blobs, intent records)
/// with a content hash that survives serde round-trips.
///
/// Note: this uses `serde_json::to_vec`, which is NOT canonical-form (map
/// key ordering depends on the input). Callers that need a stable hash
/// across serializations of the same logical value should pre-canonicalize
/// (e.g. use `BTreeMap`) before calling this helper. Both the conversation
/// crate's `propose_summary_confirmation` fingerprinter and the
/// derived-image tag generator already do this.
pub fn sha256_of_serialize<T: Serialize>(v: &T) -> anyhow::Result<String> {
    let bytes = serde_json::to_vec(v)?;
    Ok(sha256_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_of_empty_input() {
        // Well-known SHA-256("") test vector.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_hex_of_abc() {
        // Well-known SHA-256("abc") test vector.
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_hex_is_deterministic() {
        let a = sha256_hex(b"the quick brown fox");
        let b = sha256_hex(b"the quick brown fox");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn sha256_short_truncates_to_n() {
        let full = sha256_hex(b"hello");
        assert_eq!(
            sha256_short(b"hello", 8),
            full.chars().take(8).collect::<String>()
        );
        assert_eq!(sha256_short(b"hello", 16).len(), 16);
        assert_eq!(sha256_short(b"hello", 0), "");
    }

    #[test]
    fn sha256_short_saturates_at_full_length() {
        let v = sha256_short(b"hello", 9999);
        assert_eq!(v.len(), 64);
        assert_eq!(v, sha256_hex(b"hello"));
    }

    #[test]
    fn sha256_of_serialize_stable_for_same_value() {
        #[derive(serde::Serialize)]
        struct Sample {
            a: u32,
            b: String,
        }
        let v = Sample {
            a: 7,
            b: "hi".to_string(),
        };
        let h1 = sha256_of_serialize(&v).unwrap();
        let h2 = sha256_of_serialize(&v).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn sha256_of_serialize_differs_for_different_values() {
        #[derive(serde::Serialize)]
        struct Sample {
            a: u32,
        }
        let h1 = sha256_of_serialize(&Sample { a: 1 }).unwrap();
        let h2 = sha256_of_serialize(&Sample { a: 2 }).unwrap();
        assert_ne!(h1, h2);
    }
}
