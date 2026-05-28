//! Reads the 8 ECAA subgraph sidecars from a package root.

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
/// LoadedPackage data.
pub struct LoadedPackage {
    /// Intake.
    pub intake: Vec<Value>, // intake-conversation.jsonl
    /// Decisions.
    pub decisions: Vec<Value>, // decisions.jsonl
    /// Validation reports.
    pub validation_reports: Vec<Value>, // validation-reports.jsonl
    /// Proofs.
    pub proofs: Vec<Value>, // proofs.jsonl
    /// Claims.
    pub claims: Option<Value>, // claim-verification.json
    /// Verifier decisions.
    pub verifier_decisions: Vec<Value>, // verifier-decisions.jsonl
    /// Assumptions.
    pub assumptions: Vec<Value>, // assumptions.jsonl
    /// Determinism shim.
    pub determinism_shim: Option<Value>, // determinism-shim.json
    /// Security policy.
    pub security_policy: Option<Value>, // security-policy.json
    /// Plot affordances.
    pub plot_affordances: Option<Vec<Value>>, // plot_affordances.jsonl (optional)
}

impl LoadedPackage {
    /// From root.
    pub fn from_root(root: &Path) -> Result<Self> {
        let rt = root.join("runtime");
        Ok(Self {
            intake: load_jsonl_opt(&rt.join("intake-conversation.jsonl"))?.unwrap_or_default(),
            decisions: load_jsonl_opt(&rt.join("decisions.jsonl"))?.unwrap_or_default(),
            validation_reports: load_jsonl_opt(&rt.join("validation-reports.jsonl"))?
                .unwrap_or_default(),
            proofs: load_jsonl_opt(&rt.join("proofs.jsonl"))?.unwrap_or_default(),
            claims: load_json_opt(&rt.join("claim-verification.json"))?,
            verifier_decisions: load_jsonl_opt(&rt.join("verifier-decisions.jsonl"))?
                .unwrap_or_default(),
            assumptions: load_jsonl_opt(&rt.join("assumptions.jsonl"))?.unwrap_or_default(),
            determinism_shim: load_json_opt(&rt.join("determinism-shim.json"))?,
            security_policy: load_json_opt(&rt.join("security-policy.json"))?,
            plot_affordances: load_jsonl_opt(&rt.join("plot_affordances.jsonl"))?,
        })
    }
}

fn load_json_opt(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let v: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(v))
}

fn load_jsonl_opt(path: &Path) -> Result<Option<Vec<Value>>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut out = Vec::new();
    for (lineno, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .with_context(|| format!("parse {}:{}", path.display(), lineno + 1))?;
        out.push(v);
    }
    Ok(Some(out))
}
