// Emit, sidecar, and schema integration tests.
// Each submodule corresponds to a formerly top-level tests/*.rs file.
// `common` is declared once here and shared across submodules that need it.

#[path = "../common/mod.rs"]
mod common;

mod audit_proof_sidecar_emit;
mod conventional_mode_emission;
mod emit_race;
mod emit_roundtrip_schema_clean;
mod emit_validation_smoke;
mod literature_ro_crate;
mod opaque_sink_integration;
mod schemars_generation;
mod sidecar_emission;
