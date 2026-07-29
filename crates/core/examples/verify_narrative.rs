//! Standalone claim-verification harness: run ECAA's own claim verifier on an
//! ARBITRARY narrative + result table (e.g. the bare/unscaffolded arm's report),
//! using the same extractor config + interpretation policy as the live pipeline.
//! Usage: cargo run --example verify_narrative -- <narrative.md> <tables_dir> <config_dir> [class]
use ecaa_workflow_core::claim_extractor::{extract_claims, ExtractorConfig};
use ecaa_workflow_core::claim_verifier::{verify_claims, ClaimStatus};
use ecaa_workflow_core::project_class::ProjectClass;
use std::path::Path;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (nar, tables, cfgdir) = (&a[1], &a[2], &a[3]);
    let narrative = std::fs::read_to_string(nar).expect("read narrative");
    let policy_path = Path::new(cfgdir).join("downstream-policy/interpretation-policy.json");
    let policy: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&policy_path).expect("read policy"))
            .expect("parse policy");
    let cfg = ExtractorConfig::from_policy_for_class(
        &policy,
        Path::new(cfgdir),
        ProjectClass::Bioinformatics,
    )
    .expect("build extractor config");
    let claims = extract_claims(&narrative, &cfg);
    let report = verify_claims(&claims, Path::new(tables), &cfg);
    println!(
        "=== claim-verification on {} (table dir {}) ===",
        nar, tables
    );
    println!("extracted_claims={}  checked={} verified={} mismatch={} suspicious={} unverifiable={} pending={}",
        claims.len(), report.n_checked, report.n_verified, report.n_mismatch,
        report.n_suspicious, report.n_unverifiable, report.n_pending);
    println!("--- per-claim verdicts ---");
    for v in &report.verdicts {
        let tag = match &v.status {
            ClaimStatus::Verified => "VERIFIED".to_string(),
            ClaimStatus::Mismatch { detail } => format!("MISMATCH: {detail}"),
            ClaimStatus::Suspicious { reason } => format!("SUSPICIOUS: {reason}"),
            ClaimStatus::Unverifiable { reason } => format!("unverifiable: {reason}"),
            ClaimStatus::Pending { reason } => format!("pending: {reason}"),
        };
        // print the claim excerpt compactly
        let c = format!("{:?}", v.claim);
        let c = if c.len() > 160 {
            format!("{}…", &c[..160])
        } else {
            c
        };
        println!("[{tag}]\n    {c}");
    }
}
