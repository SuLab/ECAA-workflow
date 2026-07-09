//! Grant v19 §Authentication of Key Resources + §Aim 3A Arm B′ —
//! capture the determinism-relevant environment in effect at package
//! emit time. Emitted as `runtime/determinism-shim.json` by the
//! conversation crate's `emit::sidecars::write_determinism_shim`.
//!
//! The shim records:
//! - which `TZ`/`LANG`/`LC_ALL`/`PYTHONHASHSEED`/`SOURCE_DATE_EPOCH`
//!   env vars are set at emit time (values not captured for privacy,
//!   just presence + locale/timezone resolution);
//! - which secret env vars are present and redacted from the capture
//!   (recorded by name only — never values);
//! - the seed policy (SOURCE_DATE_EPOCH if set, else "process-default");
//! - the temp-path strategy + a DETERMINISTIC symbolic root. The root
//!   deliberately does NOT capture the host `$TMPDIR` (e.g.
//!   `/tmp/claude-1000`): that value is host-specific and would leak
//!   host state into the package, defeating the byte-reproducibility
//!   intent. The harness always places per-task scratch under the
//!   package-relative `runtime/scratch/<task_id>` regardless of host
//!   `$TMPDIR`, so the symbolic root is both accurate and deterministic;
//! - the active locale + timezone;
//! - the `ablation_engaged` flag mirroring
//!   [`crate::ablation::AblationFlag::ReexecutionClass`].

use crate::ablation::{AblationFlag, AblationFlagExt};
use serde::{Deserialize, Serialize};
use std::env;

/// Top-level payload for `runtime/determinism-shim.json`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeterminismShimSidecar {
    /// Schema version.
    pub schema_version: String,
    /// Env capture.
    pub env_capture: EnvCapture,
    /// Seed policy.
    pub seed_policy: SeedPolicy,
    /// Temp path policy.
    pub temp_path_policy: TempPathPolicy,
    /// Locale.
    pub locale: String,
    /// Timezone.
    pub timezone: String,
    /// Mirrors `ECAA_ABLATE_REEXECUTION_CLASS` per Subsystem B6.
    ///
    /// When `true`, the deterministic re-execution class is suppressed on
    /// the emit side (Arm B′ control). This bool flip is retained for
    /// backwards-compatibility and historical-session readers; the load-bearing
    /// suppression that empties `per_artifact` lives in
    /// `crates/conversation::emit::sidecars::write_reexecution_sidecar`.
    pub ablation_engaged: bool,

    /// Per-artifact / per-column non-determinism acknowledgments.
    ///
    /// This is the SINGLE SOURCE that the re-execution comparator
    /// (`crate::reexecution::classify_reexecution`) AND the audit-proof
    /// equivalence-failure invariant
    /// (`crate::audit_proof::invariants::equivalence_failure`) both read: a
    /// divergence that exceeds the semantic band is only bucketed
    /// `AcknowledgedNonDeterminism` (comparator) / satisfied (invariant) when a
    /// matching ack COVERS every diverging column here — a whole-artifact ack
    /// (`columns: None`) covers everything, a column-scoped ack covers only its
    /// listed columns. An undeclared divergence FAILS.
    ///
    /// Old shims that predate this field deserialize to an empty vec via serde
    /// `default`. An empty list is `skip_serializing_if`-omitted so the
    /// compiler-host snapshot stays byte-identical to pre-field shims (the
    /// byte-reproducibility contract — `determinism-shim.json` is NOT in the
    /// byte-diff exclusion set). `serialize_active_settings` emits it empty; the
    /// emit-time projection (atom `non_determinism` decl → this field) is wired
    /// separately and should assign through
    /// [`DeterminismShimSidecar::set_non_deterministic_artifacts`] to stay
    /// byte-stable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_deterministic_artifacts: Vec<NonDetAck>,
}

/// One acknowledged source of non-determinism, scoped to an artifact and
/// (optionally) a set of columns within it.
///
/// The `artifact` string may be a bare filename (`de_results.tsv`) or a
/// package-relative path
/// (`runtime/outputs/differential_expression/de_results.tsv`);
/// [`ack_for`] matches either form against a re-execution outcome id.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct NonDetAck {
    /// Artifact this ack applies to (bare filename or package-relative path).
    pub artifact: String,
    /// Columns within the artifact whose divergence is acknowledged. `None`
    /// (absent) is a WHOLE-ARTIFACT ack covering every column; `Some(cols)`
    /// covers only the named columns, so a divergence in an un-listed column
    /// still fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<String>>,
    /// The class of non-determinism.
    pub kind: NonDetKind,
    /// Human-readable justification (surfaced in provenance; never parsed).
    pub reason: String,
}

/// Class of an acknowledged non-determinism source. `#[non_exhaustive]` per
/// the public-enum SemVer contract — adding a future class is a non-breaking
/// minor change for downstream RO-Crate / schema consumers.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NonDetKind {
    /// Order-dependent multithreaded reduction (e.g. thread-count-sensitive
    /// BLAS sum ordering) that changes low-order bits run-to-run.
    MultithreadedReduction,
    /// An RNG that was never seeded, so draws differ across runs.
    UnseededRng,
    /// Empirical-Bayes / adaptive shrinkage whose fitted prior shifts the
    /// shrunken estimate within the tolerance band (e.g. DESeq2/apeglm
    /// log2 fold-change shrinkage).
    AdaptiveShrinkage,
    /// Floating-point non-associativity across differing summation orders.
    FloatingPointAssociativity,
    /// Any other declared, justified source.
    Other,
}

/// Determinism-relevant env vars: presence-captured (never value-captured)
/// + secret env vars marked as "redacted".
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EnvCapture {
    /// Captured env vars.
    pub captured_env_vars: Vec<String>,
    /// Redacted env vars.
    pub redacted_env_vars: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// SeedPolicy data.
pub struct SeedPolicy {
    /// Random seed.
    pub random_seed: Option<u64>,
    /// Seed source.
    pub seed_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// TempPathPolicy data.
pub struct TempPathPolicy {
    /// Strategy.
    pub strategy: String,
    /// Root.
    pub root: String,
}

const CAPTURED_ENV_VARS: &[&str] = &[
    "TZ",
    "LANG",
    "LC_ALL",
    "PYTHONHASHSEED",
    "SOURCE_DATE_EPOCH",
];

const REDACTED_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ECAA_ANTHROPIC_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
];

/// Snapshot the active determinism environment. Reads from `std::env`
/// — never opens a file or makes a network call. Pure with respect to
/// its env-var inputs.
///
/// This is the COMPILER-HOST snapshot. Container-resolved determinism
/// env (what the five seed/locale vars actually were INSIDE the
/// execution container, recorded per-task by the agent wrapper as
/// `runtime/outputs/<task_id>/determinism-env.json`) is merged in at
/// finalize via [`merge_container_env`].
pub fn serialize_active_settings() -> DeterminismShimSidecar {
    DeterminismShimSidecar {
        schema_version: "1".into(),
        env_capture: EnvCapture {
            captured_env_vars: CAPTURED_ENV_VARS
                .iter()
                .filter(|k| env::var(k).is_ok())
                .map(|k| (*k).to_string())
                .collect(),
            redacted_env_vars: REDACTED_ENV_VARS
                .iter()
                .filter(|k| env::var(k).is_ok())
                .map(|k| (*k).to_string())
                .collect(),
        },
        seed_policy: SeedPolicy {
            random_seed: env::var("SOURCE_DATE_EPOCH")
                .ok()
                .and_then(|v| v.parse().ok()),
            seed_source: if env::var("SOURCE_DATE_EPOCH").is_ok() {
                "SOURCE_DATE_EPOCH".into()
            } else {
                "process-default".into()
            },
        },
        temp_path_policy: TempPathPolicy {
            strategy: "stable-by-task-id".into(),
            // DETERMINISTIC by design: the harness scratch root is the
            // package-relative `runtime/scratch/<task_id>`, never the
            // host `$TMPDIR`. Capturing the literal `$TMPDIR`
            // (e.g. `/tmp/claude-1000`) would bake host state into the
            // sidecar and break byte-reproducibility, so we record the
            // symbolic package-relative root instead.
            root: "runtime/scratch".into(),
        },
        locale: env::var("LC_ALL")
            .or_else(|_| env::var("LANG"))
            .unwrap_or_else(|_| "C".into()),
        timezone: env::var("TZ").unwrap_or_else(|_| "UTC".into()),
        ablation_engaged: AblationFlag::ReexecutionClass.is_active(),
        // The compiler-host snapshot never declares acks — they are projected
        // from atom `non_determinism` declarations at emit time by a separate
        // pass. Empty here keeps the host snapshot byte-stable.
        non_deterministic_artifacts: Vec::new(),
    }
}

impl DeterminismShimSidecar {
    /// Assign the non-determinism acknowledgments, keeping them sorted +
    /// deduplicated for byte-stable serialization. The emit-time projection
    /// (atom `non_determinism` decl → this field) calls this so the sidecar is
    /// reproducible regardless of the order atoms were visited.
    pub fn set_non_deterministic_artifacts(&mut self, mut acks: Vec<NonDetAck>) {
        acks.sort();
        acks.dedup();
        self.non_deterministic_artifacts = acks;
    }
}

/// Return the acknowledgment that covers `artifact_path`, if the shim declares
/// one. Matching is exact on the declared `artifact` string, then by trailing
/// path segment, then by file name — so a shim that declares a bare
/// `de_results.tsv` still matches a re-execution outcome id of
/// `runtime/outputs/differential_expression/de_results.tsv`.
///
/// This is the shared lookup both the comparator and the equivalence-failure
/// invariant use, so the comparator's `AcknowledgedNonDeterminism` bucket and
/// the invariant's satisfied verdict are driven by the SAME declaration.
pub fn ack_for<'a>(shim: &'a DeterminismShimSidecar, artifact_path: &str) -> Option<&'a NonDetAck> {
    shim.non_deterministic_artifacts
        .iter()
        .find(|a| artifact_ack_matches(&a.artifact, artifact_path))
}

/// True when a shim-declared `artifact` string identifies the same artifact as
/// a re-execution outcome id (`artifact_path`).
fn artifact_ack_matches(declared: &str, artifact_path: &str) -> bool {
    if declared == artifact_path {
        return true;
    }
    // Trailing path-segment match: artifact_path == ".../<declared>".
    if let Some(prefix_len) = artifact_path.len().checked_sub(declared.len()) {
        if artifact_path.ends_with(declared) && artifact_path[..prefix_len].ends_with('/') {
            return true;
        }
    }
    // File-name match: a bare filename declared against a full relative path.
    let file_name = |p: &str| {
        std::path::Path::new(p)
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
    };
    match (file_name(declared), file_name(artifact_path)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Fold a per-task container's captured determinism env keys into the
/// package-level (compiler-host) shim's `captured_env_vars`, as a
/// deterministic `BTreeSet` union (deduped + sorted). The host shim
/// stays a pure emit-time snapshot; this is the seam where
/// harness-written runtime evidence
/// (`runtime/outputs/<task_id>/determinism-env.json`, D4) augments it
/// at finalize. Byte-stable: identical inputs yield identical output
/// ordering.
pub fn merge_container_env(host: &mut DeterminismShimSidecar, container_keys: &[String]) {
    let mut set: std::collections::BTreeSet<String> =
        host.env_capture.captured_env_vars.iter().cloned().collect();
    set.extend(container_keys.iter().cloned());
    host.env_capture.captured_env_vars = set.into_iter().collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shim_has_schema_v1() {
        let s = serialize_active_settings();
        assert_eq!(s.schema_version, "1");
    }

    #[test]
    fn temp_path_policy_is_stable_by_task_id() {
        let s = serialize_active_settings();
        assert_eq!(s.temp_path_policy.strategy, "stable-by-task-id");
    }

    #[test]
    #[serial_test::serial]
    fn temp_path_root_is_deterministic_and_does_not_leak_host_tmpdir() {
        // det-07 — the temp-path root must be a deterministic,
        // package-relative symbolic value, never the host `$TMPDIR`.
        // Set a bogus TMPDIR and assert the sidecar root ignores it.
        // `serial_test::serial` keeps this off the other env-mutating
        // suites; restore the prior value before returning.
        let prev = env::var("TMPDIR").ok();
        env::set_var("TMPDIR", "/tmp/claude-99999");
        let s = serialize_active_settings();
        assert_eq!(
            s.temp_path_policy.root, "runtime/scratch",
            "temp_path_policy.root must be the deterministic package-relative \
             scratch root, not the host $TMPDIR"
        );
        assert!(
            !s.temp_path_policy.root.contains("/tmp/"),
            "host $TMPDIR leaked into determinism-shim: {}",
            s.temp_path_policy.root
        );
        match prev {
            Some(v) => env::set_var("TMPDIR", v),
            None => env::remove_var("TMPDIR"),
        }
    }

    #[test]
    fn merge_container_env_unions_keys_deterministically() {
        let mut host = serialize_active_settings();
        host.env_capture.captured_env_vars = vec!["TZ".to_string()];
        // Container reported these set inside the container (out of order).
        let container = vec![
            "LC_ALL".to_string(),
            "PYTHONHASHSEED".to_string(),
            "TZ".to_string(), // duplicate must not double
        ];
        merge_container_env(&mut host, &container);
        assert_eq!(
            host.env_capture.captured_env_vars,
            vec![
                "LC_ALL".to_string(),
                "PYTHONHASHSEED".to_string(),
                "TZ".to_string()
            ],
            "merge must dedupe + sort (BTreeSet union) for byte-stability"
        );
    }

    #[test]
    fn host_snapshot_has_empty_acks() {
        let s = serialize_active_settings();
        assert!(
            s.non_deterministic_artifacts.is_empty(),
            "the compiler-host snapshot must never declare acks"
        );
    }

    #[test]
    fn old_shim_without_field_deserializes_to_empty_acks() {
        // A shim written before the field existed must load with an empty ack
        // set via serde default, not fail to parse.
        let json = "{\"schema_version\":\"1\",\"env_capture\":{\"captured_env_vars\":[],\
            \"redacted_env_vars\":[]},\"seed_policy\":{\"random_seed\":null,\
            \"seed_source\":\"process-default\"},\"temp_path_policy\":{\
            \"strategy\":\"stable-by-task-id\",\"root\":\"runtime/scratch\"},\
            \"locale\":\"C\",\"timezone\":\"UTC\",\"ablation_engaged\":false}";
        let shim: DeterminismShimSidecar =
            serde_json::from_str(json).expect("old shim must deserialize");
        assert!(shim.non_deterministic_artifacts.is_empty());
    }

    #[test]
    fn set_acks_sorts_and_dedups() {
        let mut shim = serialize_active_settings();
        shim.set_non_deterministic_artifacts(vec![
            NonDetAck {
                artifact: "z.tsv".into(),
                columns: None,
                kind: NonDetKind::UnseededRng,
                reason: "b".into(),
            },
            NonDetAck {
                artifact: "a.tsv".into(),
                columns: Some(vec!["c1".into()]),
                kind: NonDetKind::AdaptiveShrinkage,
                reason: "a".into(),
            },
            NonDetAck {
                artifact: "a.tsv".into(),
                columns: Some(vec!["c1".into()]),
                kind: NonDetKind::AdaptiveShrinkage,
                reason: "a".into(),
            },
        ]);
        // Sorted by artifact, duplicate collapsed.
        assert_eq!(shim.non_deterministic_artifacts.len(), 2);
        assert_eq!(shim.non_deterministic_artifacts[0].artifact, "a.tsv");
        assert_eq!(shim.non_deterministic_artifacts[1].artifact, "z.tsv");
    }

    #[test]
    fn ack_for_matches_by_filename_path_suffix_and_exact() {
        let mut shim = serialize_active_settings();
        shim.set_non_deterministic_artifacts(vec![NonDetAck {
            artifact: "de_results.tsv".into(),
            columns: Some(vec!["log2FoldChange".into()]),
            kind: NonDetKind::AdaptiveShrinkage,
            reason: "shrinkage".into(),
        }]);
        // File-name match against a full relative path.
        assert!(
            ack_for(&shim, "runtime/outputs/differential_expression/de_results.tsv").is_some(),
            "bare filename ack must match a full rel path by file name"
        );
        // Exact match.
        assert!(ack_for(&shim, "de_results.tsv").is_some());
        // Non-match.
        assert!(ack_for(&shim, "other.tsv").is_none());
    }

    #[test]
    fn ack_for_exact_path_match() {
        let mut shim = serialize_active_settings();
        shim.set_non_deterministic_artifacts(vec![NonDetAck {
            artifact: "results/tables/de.tsv".into(),
            columns: None,
            kind: NonDetKind::Other,
            reason: "whole artifact".into(),
        }]);
        assert!(ack_for(&shim, "results/tables/de.tsv").is_some());
        // A different directory with the same basename still matches by name.
        assert!(ack_for(&shim, "elsewhere/de.tsv").is_some());
    }

    #[test]
    fn seed_source_defaults_to_process_default_when_source_date_epoch_unset() {
        // Pre-existing SOURCE_DATE_EPOCH would invalidate the
        // assertion — skip in that case rather than mutating env in a
        // non-serial test.
        if env::var("SOURCE_DATE_EPOCH").is_ok() {
            return;
        }
        let s = serialize_active_settings();
        assert_eq!(s.seed_policy.seed_source, "process-default");
        assert!(s.seed_policy.random_seed.is_none());
    }
}
