//! Compile-time `K`-parameterized k-mer (v4 D1 / F23).
//!
//! Const-generic `K` makes the k-mer length a type-level fact, so
//! `canonicalize::<31>` and `canonicalize::<21>` cannot accidentally
//! mix in the same pipeline. NEVER derive serde / ts-rs on this
//! type.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Base {
    #[allow(dead_code)]
    A,
    #[allow(dead_code)]
    C,
    #[allow(dead_code)]
    G,
    #[allow(dead_code)]
    T,
    #[allow(dead_code)]
    N,
}

/// A k-mer of length `K`. `pub(crate)` by F23 mandate.
pub(crate) struct KMer<const K: usize> {
    pub(crate) bases: [Base; K],
}

impl<const K: usize> KMer<K> {
    pub(crate) const SIZE: usize = K;

    pub(crate) fn new(bases: [Base; K]) -> Self {
        Self { bases }
    }
}

/// Return the lexicographically smaller of forward k-mer or its
/// reverse complement. The canonicalisation step lets a downstream
/// counter treat both strands as equivalent.
///
/// Placeholder real-implementation: real call sites (jellyfish-style
/// counters) live downstream of v4 P8's two-helper migration.
pub(crate) fn canonicalize<const K: usize>(k: KMer<K>) -> KMer<K> {
    // Real impl would compare against `reverse_complement(k)` and
    // return the smaller. This stub focuses on the type discipline,
    // not the de-Bruijn graph — leave the body trivial.
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kmer_carries_const_size() {
        assert_eq!(KMer::<21>::SIZE, 21);
        assert_eq!(KMer::<31>::SIZE, 31);
    }

    #[test]
    fn canonicalize_returns_same_length() {
        let k = KMer::<3>::new([Base::A, Base::C, Base::G]);
        let canon = canonicalize(k);
        assert_eq!(canon.bases.len(), 3);
    }
}
