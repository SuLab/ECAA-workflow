//! Compile-time type discipline (v4 D1 / F23).
//!
//! Closes v4 §13.4 and F23: compile-time discipline does not cross
//! the serialization boundary. The phantom-typed wrappers in this
//! module (`AlignedReads<R>`, `Interval<C>`, `PortInPass<S>`,
//! `KMer<K>`) carry type-level information that the Rust compiler
//! enforces at call sites but that has NO on-disk representation.
//!
//! Invariants:
//!
//! - The marker traits (`ReferenceGenome`, `CoordinateSystem`) and
//!   their zero-sized inhabitant types (`GRCh38`, `GRCh37`,
//!   `ZeroBasedHalfOpen`, etc.) are `pub` so internal callers can
//!   parameterize against them.
//! - The phantom-typed wrappers are `pub(crate)`-effective via the
//!   `pub(crate) mod...` declarations below. They live inside the
//!   compile-time-discipline boundary on purpose: every wrapper
//!   crossing the IR / RO-Crate / JSONL boundary would re-introduce
//!   the runtime check the type discipline is meant to replace.
//! - NEVER derive `Serialize`, `Deserialize`, or `TS` on the phantom
//!   wrappers. The compile-fail test under
//!   `crates/core/tests/compile_fail/` asserts this; F23 enforces it
//!   in CI.
//!
//! The two highest-leverage adoption sites (v4 P8) are tagged with
//! `// v4 P8`:
//!
//! - `crates/core/src/compatibility/engine.rs` — coordinate-system
//!   typestate helper consumed by the facet unification path.
//! - `crates/core/src/adapter_registry.rs` — liftover-decision typestate
//!   helper consumed by the genome-build adapter dispatch path.
//!
//! `lib.rs` declares this module with `pub mod compile_time_discipline;`
//! But does NOT add a `pub use...` re-export — every external caller
//! must spell out the full module path, which is itself blocked by
//! the `pub(crate)` submodule visibility below.

// `#[allow(dead_code)]` is load-bearing on every submodule: the
// foundation phantom types (`Interval<C>`, `KMer<K>`, `PortInPass<S>`,
// some of the `AlignedReads<R>` surface area) carry deliberately
// unused fields like `start` / `end` and unused associated functions
// so adopters who pick up the discipline incrementally don't have to
// re-author the module. F23's role is to keep the discipline
// available; v4 P8 ships two adoption sites with the rest reserved
// for follow-up phases. The compile-fail harness under
// `tests/compile_fail/` does NOT need any of the placeholder
// surface — it only imports `AlignedReads<GRCh38>` and asserts the
// derive fails — so the dead-code allowance does not weaken F23.

#[allow(dead_code)]
pub(crate) mod coordinate_system;
#[allow(dead_code)]
pub(crate) mod kmer;
#[allow(dead_code)]
pub(crate) mod pass_state;
#[allow(dead_code)]
pub(crate) mod reference_genome;
