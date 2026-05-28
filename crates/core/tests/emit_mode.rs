//! Unit tests for the `EcaaMode` parser (Aim 3A Arm B″ wiring).

use scripps_workflow_core::emit_mode::EcaaMode;

#[test]
fn default_mode_is_full() {
    assert_eq!(EcaaMode::from_env_str(None), EcaaMode::Full);
    assert_eq!(EcaaMode::from_env_str(Some("")), EcaaMode::Full);
}

#[test]
fn conventional_mode_parses() {
    assert_eq!(
        EcaaMode::from_env_str(Some("conventional")),
        EcaaMode::Conventional
    );
    assert_eq!(
        EcaaMode::from_env_str(Some("CONVENTIONAL")),
        EcaaMode::Conventional
    );
}

#[test]
fn unknown_mode_falls_back_to_full_with_warning() {
    // We accept fallback rather than panic; the warning is observable
    // via a tracing event but not asserted here.
    assert_eq!(EcaaMode::from_env_str(Some("garbage")), EcaaMode::Full);
}
