//! critical-analysis H4: the startup network warning in the harness binary must
//! not claim bubblewrap enforces egress-deny. bubblewrap only adds PROCESS
//! isolation for Exec/GeneratedCode atoms; Compute atoms (the majority) are
//! never network-checked and the local executor advertises Bridge (full
//! egress). Real per-atom network enforcement lives on the SLURM and AWS
//! executors only — the warning must point there.

#[test]
fn main_rs_warning_drops_false_bubblewrap_remediation() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("read crates/harness/src/main.rs");
    assert!(
        !src.contains("Set ECAA_LOCAL_SANDBOX=bubblewrap to enforce egress deny"),
        "the false bubblewrap-enforces-egress remediation must be removed"
    );
    assert!(
        src.contains("SLURM") || src.contains("AWS"),
        "the warning should point to the executors that actually enforce egress"
    );
}
