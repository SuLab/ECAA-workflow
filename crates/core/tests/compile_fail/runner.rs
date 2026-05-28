//! trybuild compile-fail harness for the v4 P8 F23 invariant.
//!
//! Phantom-typed compile-time-discipline wrappers
//! (`AlignedReads<R>`, `Interval<C>`, `PortInPass<S>`, `KMer<K>`)
//! MUST NOT derive `Serialize` / `Deserialize` / `TS`. The
//! companion fixture under `no_serialize_on_phantom.rs` attempts
//! such a derive; trybuild asserts that the file fails to compile.

#[test]
fn no_serialize_on_phantom_typed_structs() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/no_serialize_on_phantom.rs");
}
