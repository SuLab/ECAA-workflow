//! Phantom-typed reference-genome markers (v4 D1 / F23).
//!
//! NEVER derive `Serialize`, `Deserialize`, or `TS` on `AlignedReads<R>`
//! or any other type in this module. The trybuild compile-fail test
//! (`crates/core/tests/compile_fail/no_serialize_on_phantom.rs`) asserts
//! the invariant; F23 enforces it nightly.
//!
//! Marker traits (`ReferenceGenome`) and their inhabitant types
//! (`GRCh38`, `GRCh37`, `T2T_CHM13`, `Mm39`) are `pub` so internal
//! call sites can parameterize against them; the `AlignedReads<R>`
//! wrapper is `pub(crate)` because it MUST NOT escape the compile-
//! time-discipline boundary.

#![allow(unreachable_pub)]
use std::marker::PhantomData;

/// Marker trait identifying a reference-genome build at the type
/// level. Implementors are zero-sized; they exist only so the
/// compiler can prove that two `AlignedReads<R>` values share the
/// same coordinate space.
pub trait ReferenceGenome: 'static + Send + Sync {
    const NAME: &'static str;
}

/// Zero-sized marker for GRCh38 (current human reference).
pub struct GRCh38;
/// Zero-sized marker for GRCh37 (legacy human reference).
pub struct GRCh37;
/// Zero-sized marker for T2T-CHM13 (complete human genome).
#[allow(non_camel_case_types)]
pub struct T2T_CHM13;
/// Zero-sized marker for Mm39 (current mouse reference).
pub struct Mm39;

impl ReferenceGenome for GRCh38 {
    const NAME: &'static str = "GRCh38";
}
impl ReferenceGenome for GRCh37 {
    const NAME: &'static str = "GRCh37";
}
impl ReferenceGenome for T2T_CHM13 {
    const NAME: &'static str = "T2T_CHM13";
}
impl ReferenceGenome for Mm39 {
    const NAME: &'static str = "Mm39";
}

/// Aligned-reads bundle parameterized by reference genome `R`.
///
/// The phantom `R` prevents the compiler from accepting an
/// `AlignedReads<GRCh37>` in a slot that expects an
/// `AlignedReads<GRCh38>` — the most common source of silent
/// coordinate-system bugs in genome workflows.
///
/// `pub(crate)` by F23 mandate: this type carries compile-time
/// information that has no on-disk representation. Serializing it
/// would round-trip through a runtime string and erase the
/// phantom-typed guarantee.
pub(crate) struct AlignedReads<R: ReferenceGenome> {
    _phantom: PhantomData<R>,
    pub(crate) coordinate_sorted: bool,
    pub(crate) indexed: bool,
}

impl<R: ReferenceGenome> AlignedReads<R> {
    pub(crate) fn new(coordinate_sorted: bool, indexed: bool) -> Self {
        Self {
            _phantom: PhantomData,
            coordinate_sorted,
            indexed,
        }
    }

    pub(crate) fn reference_name() -> &'static str {
        R::NAME
    }
}

/// Liftover dispatch helper: returns `true` when the source
/// reference `F` differs from the target reference `T`. The
/// caller must specify both types explicitly; this is the
/// load-bearing detail that prevents accidental same-reference
/// liftover.
pub(crate) fn liftover_required<F: ReferenceGenome, T: ReferenceGenome>(
    _from: &AlignedReads<F>,
    _to: PhantomData<T>,
) -> bool {
    !type_eq::<F, T>()
}

fn type_eq<A: 'static, B: 'static>() -> bool {
    std::any::TypeId::of::<A>() == std::any::TypeId::of::<B>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_genome_marker_names_are_stable() {
        assert_eq!(GRCh38::NAME, "GRCh38");
        assert_eq!(GRCh37::NAME, "GRCh37");
        assert_eq!(T2T_CHM13::NAME, "T2T_CHM13");
        assert_eq!(Mm39::NAME, "Mm39");
    }

    #[test]
    fn aligned_reads_carries_reference_name() {
        assert_eq!(AlignedReads::<GRCh38>::reference_name(), "GRCh38");
        assert_eq!(AlignedReads::<GRCh37>::reference_name(), "GRCh37");
    }

    #[test]
    fn liftover_required_when_references_differ() {
        let reads: AlignedReads<GRCh37> = AlignedReads::new(true, true);
        assert!(liftover_required(&reads, PhantomData::<GRCh38>));
        assert!(!liftover_required(&reads, PhantomData::<GRCh37>));
    }
}
