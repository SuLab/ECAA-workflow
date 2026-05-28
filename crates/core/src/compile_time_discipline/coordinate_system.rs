//! Phantom-typed coordinate-system markers (v4 D1 / F23).
//!
//! NEVER derive `Serialize`, `Deserialize`, or `TS` on `Interval<C>`
//! or any other type in this module. The trybuild compile-fail test
//! enforces this; F23 catches drift in CI.
//!
//! The marker trait (`CoordinateSystem`) and inhabitant types
//! (`ZeroBasedHalfOpen`, `OneBasedClosed`) are `pub` so internal
//! callers can parameterize against them; the `Interval<C>` wrapper
//! is `pub(crate)` because it MUST NOT escape the compile-time-
//! discipline boundary.

#![allow(unreachable_pub)]
use std::marker::PhantomData;

/// Marker trait identifying a coordinate system at the type
/// level. Distinguishing 0-based half-open (BED, VCF positions
/// minus 1) from 1-based closed (GFF, VCF positions) at compile
/// time prevents the off-by-one bugs that flooded variant-calling
/// pipelines for the better part of a decade.
pub trait CoordinateSystem: 'static + Send + Sync {
    const NAME: &'static str;
    const IS_ZERO_BASED: bool;
    const IS_HALF_OPEN: bool;
}

/// Zero-sized marker for 0-based half-open intervals (BED, BAM,
/// `seqan::Interval`).
pub struct ZeroBasedHalfOpen;
/// Zero-sized marker for 1-based closed intervals (GFF3, VCF,
/// SAM CIGAR).
pub struct OneBasedClosed;

impl CoordinateSystem for ZeroBasedHalfOpen {
    const NAME: &'static str = "0-based half-open";
    const IS_ZERO_BASED: bool = true;
    const IS_HALF_OPEN: bool = true;
}
impl CoordinateSystem for OneBasedClosed {
    const NAME: &'static str = "1-based closed";
    const IS_ZERO_BASED: bool = false;
    const IS_HALF_OPEN: bool = false;
}

/// Genomic interval parameterized by coordinate system `C`.
///
/// The phantom `C` makes mixing a BED-derived `Interval<ZeroBasedHalfOpen>`
/// with a GFF-derived `Interval<OneBasedClosed>` a compile error,
/// not a runtime bug.
///
/// `pub(crate)` by F23 mandate.
pub(crate) struct Interval<C: CoordinateSystem> {
    _phantom: PhantomData<C>,
    pub(crate) start: u64,
    pub(crate) end: u64,
}

impl<C: CoordinateSystem> Interval<C> {
    pub(crate) fn new(start: u64, end: u64) -> Self {
        Self {
            _phantom: PhantomData,
            start,
            end,
        }
    }

    pub(crate) fn coordinate_system_name() -> &'static str {
        C::NAME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinate_system_markers_disclose_axes() {
        assert!(ZeroBasedHalfOpen::IS_ZERO_BASED);
        assert!(ZeroBasedHalfOpen::IS_HALF_OPEN);
        assert!(!OneBasedClosed::IS_ZERO_BASED);
        assert!(!OneBasedClosed::IS_HALF_OPEN);
    }

    #[test]
    fn interval_carries_coordinate_system_name() {
        assert_eq!(
            Interval::<ZeroBasedHalfOpen>::coordinate_system_name(),
            "0-based half-open"
        );
        assert_eq!(
            Interval::<OneBasedClosed>::coordinate_system_name(),
            "1-based closed"
        );
    }

    #[test]
    fn interval_stores_endpoints() {
        let bed: Interval<ZeroBasedHalfOpen> = Interval::new(10, 20);
        assert_eq!(bed.start, 10);
        assert_eq!(bed.end, 20);
    }
}
