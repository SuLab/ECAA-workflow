//! F23: no phantom-typed struct may derive serde traits.
//!
//! The compile-time test under `crates/core/tests/compile_fail/`
//! is the real enforcement (trybuild asserts the
//! `#[derive(Serialize)] struct ShouldNotCompile(AlignedReads<GRCh38>)`
//! fixture fails to compile). This file is a runtime defense-in-
//! depth check: it walks a well-known public IR type name and
//! asserts that the compile-time-discipline phantom wrappers
//! (`AlignedReads<R>`, `Interval<C>`) never leak into the
//! externally-visible IR.
//!
//! If a contributor accidentally re-exports
//! `compile_time_discipline::reference_genome::AlignedReads` via a
//! `pub use...` or widens a `pub(crate)` to `pub` in the wrapper
//! modules, the public IR `type_name` paths will start to mention
//! `AlignedReads` and the assertion below fails.

#[test]
fn phantom_module_does_not_leak_into_lib_re_exports() {
    // Pick a load-bearing externally-visible IR type and assert
    // that its `type_name` makes no reference to the phantom-typed
    // wrappers from `crate::compile_time_discipline`. Re-exporting
    // those wrappers would cause monomorphised generic IR types to
    // mention them in their fully-qualified names.
    let path = std::any::type_name::<ecaa_workflow_core::workflow_contracts::task_node::TaskNode>();
    assert!(
        !path.contains("AlignedReads"),
        "phantom-typed `AlignedReads<R>` leaked into the public IR via TaskNode: {path}",
    );
    assert!(
        !path.contains("Interval<"),
        "phantom-typed `Interval<C>` leaked into the public IR via TaskNode: {path}",
    );
}
