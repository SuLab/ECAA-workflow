//! F23: phantom-typed compile-time-discipline wrappers must NOT
//! derive `Serialize`. This file is consumed by trybuild's
//! compile-fail harness; it MUST NOT compile.
//!
//! Two failure modes are equally valid (either proves F23):
//!
//! 1. The import fails because `compile_time_discipline::reference_genome`
//! is `pub(crate)` and not reachable from external code (the
//! by-construction discipline of v4 §13.4).
//!
//! 2. `#[derive(Serialize)]` fails because `AlignedReads<R>` contains
//! `PhantomData<R: ReferenceGenome>` and a non-`Serialize` field,
//! so the derive macro cannot synthesize an impl.
//!
//! The runner under `crates/core/tests/compile_fail/runner.rs`
//! asserts only that this file fails to compile — F23's invariant
//! is preserved regardless of which arm fires.

use scripps_workflow_core::compile_time_discipline::reference_genome::{AlignedReads, GRCh38};
use serde::Serialize;

#[derive(Serialize)]
struct ShouldNotCompile(AlignedReads<GRCh38>);

fn main() {}
