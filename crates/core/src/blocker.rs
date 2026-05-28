//! Closed taxonomy of reasons a task or chat session can land in a
//! Blocked state. Types live in `crates/ecaa-types/src/blocker.rs` (the
//! canonical ECAA v0.1 binding) and are re-exported here so existing
//! `ecaa_workflow_core::blocker::*` consumers continue to compile
//! unchanged.
//!
//! Helper functions that map the agent's free-form reason text into a
//! typed `BlockerKind` (`parse_agent_blocker_kind`,
//! `format_safety_policy_marker`) stay in core — they are
//! compiler-side glue, not part of the wire-shape binding the spec
//! crate ships.

pub use ecaa_workflow_types::blocker::{
    BlockerContext, BlockerEntry, BlockerKind, ExcludedPath, LiteratureClaimFailureKind,
    SandboxRefusalRecord, StallAction, StallSignalWire, ValidationFailureCause,
};

/// Tolerant mapper that turns the agent's free-form `blocker_kind`
/// string (read from `runtime/outputs/<task>/blocker.json`) into a
/// typed [`BlockerKind`]. The server's `/progress` handler calls this
/// before promoting a `HarnessTaskBlocked` event to session state so
/// the BlockerCard gets the right shape.
///
/// Rules:
/// - Known strings map to their typed variant, pulling detail fields
///   from `blocker_json` when present.
/// - Unknown strings fall through to
///   [`BlockerKind::AwaitingStructuredDecision`] with a logged
///   `[blocker_kind_unknown] …` stderr line so operators can notice
///   agent vocabulary drift.
/// - Missing / unparseable `blocker_json` is not fatal — we still
///   produce a structured bucket with the `summary` pulled from the
///   reason text.
///
/// `reason` is the free-form message the agent put in the
/// `record.reason` field — used as the summary fallback.
pub fn parse_agent_blocker_kind(
    raw_kind: &str,
    task_id: &str,
    reason: &str,
    blocker_json: Option<&serde_json::Value>,
) -> BlockerKind {
    parse_agent_blocker_kind_with_envelope(raw_kind, task_id, reason, blocker_json, None)
}

/// Render a `BlockerKind` from the dispatch-time safety
/// gate (`SandboxRequired`, `NetworkPolicyMismatch`,
/// `ProvisioningDenied`) into the `BlockedRecord.reason` marker the
/// harness writes. [`parse_agent_blocker_kind`] round-trips this exact
/// shape back into the typed variant when the server promotes the
/// blocker for the UI. Returns `None` when `kind` is not one of the
/// three §A.S6 dispatch-time variants — callers should fall back to a
/// plain-text reason in that case.
pub fn format_safety_policy_marker(kind: &BlockerKind) -> Option<String> {
    let prefix = match kind {
        BlockerKind::SandboxRequired { .. } => "[sandbox_required]",
        BlockerKind::NetworkPolicyMismatch { .. } => "[network_policy_mismatch]",
        BlockerKind::ProvisioningDenied { .. } => "[provisioning_denied]",
        BlockerKind::ControlledAccessViolation { .. } => "[controlled_access_violation]",
        _ => return None,
    };
    let payload = serde_json::to_string(kind).ok()?;
    Some(format!("{prefix} {payload}"))
}

/// Variant of [`parse_agent_blocker_kind`] that consumes a parsed
/// `ToolErrorEnvelope` when one is present at
/// `runtime/outputs/<task>/error.json`. When the envelope is `Some`
/// the mapping always promotes to `BlockerKind::ToolError` regardless
/// of `raw_kind` — structured tool-failure capture is strictly more
/// informative than any agent string.
pub fn parse_agent_blocker_kind_with_envelope(
    raw_kind: &str,
    task_id: &str,
    reason: &str,
    blocker_json: Option<&serde_json::Value>,
    envelope: Option<crate::error_envelope::ToolErrorEnvelope>,
) -> BlockerKind {
    if let Some(envelope) = envelope {
        return BlockerKind::ToolError {
            envelope: Box::new(envelope),
        };
    }

    let summary = one_line_summary(reason, 160);
    let decision_points_path = format!("runtime/outputs/{}/blocker.json", task_id);

    // Harness-emitted silent-completion extension.
    // The silent-completion guard writes reasons of the form
    // `[missing_artifact] task=<id> paths=<csv> — …`
    // so the mapper upgrades these to `BlockerKind::MissingArtifact`
    // before any string-kind dispatch runs. Agent-side `raw_kind` is
    // unused for this path because the guard is server-less.
    if let Some(missing_csv) = reason
        .strip_prefix("[missing_artifact]")
        .and_then(|s| s.trim_start().split("paths=").nth(1))
        .and_then(|s| s.split_whitespace().next())
    {
        let missing_paths: Vec<String> = missing_csv
            .split(',')
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .collect();
        return BlockerKind::MissingArtifact {
            task_id: task_id.to_string(),
            missing_paths,
        };
    }

    // Heartbeat stall marker. The container-aware reaper (S15.22) emits
    // an upgraded marker `[container_hung] task=<id> age_secs=<N>
    // container_id=<id> runtime=<docker|podman|apptainer>` when an SSM /
    // SSH probe confirms the container is still alive on a healthy host
    // — the in-container agent is wedged but the instance is fine, so
    // the SME's recovery affordance is "reap container only and rerun
    // on the same host" rather than "tear the host down". When the
    // probe yields no signal, the legacy `[heartbeat_stalled]` prefix
    // is preserved.
    if let Some(rest) = reason.strip_prefix("[container_hung]") {
        let mut age = 0u64;
        let mut container_id = String::new();
        let mut runtime = String::new();
        for tok in rest.split_whitespace() {
            if let Some(v) = tok.strip_prefix("age_secs=") {
                age = v.parse::<u64>().unwrap_or(0);
            } else if let Some(v) = tok.strip_prefix("container_id=") {
                container_id = v.to_string();
            } else if let Some(v) = tok.strip_prefix("runtime=") {
                runtime = v.to_string();
            }
        }
        return BlockerKind::ContainerHung {
            task_id: task_id.to_string(),
            container_id,
            runtime,
            last_heartbeat_secs_ago: age,
        };
    }

    if let Some(rest) = reason.strip_prefix("[heartbeat_stalled]") {
        let age = rest
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("age_secs="))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        return BlockerKind::HeartbeatStalled {
            task_id: task_id.to_string(),
            last_heartbeat_secs_ago: age,
        };
    }

    // Iterate-until convergence failure marker. Format:
    // `[iteration_did_not_converge] task=<id> iterations_run=<n>
    // last_metric=<f> threshold=<f> — <human prose>`
    // The agent emits this when the iterate atom hits `max_iterations`
    // without satisfying the convergence rule.
    if let Some(rest) = reason.strip_prefix("[iteration_did_not_converge]") {
        let mut iterations_run: u32 = 0;
        let mut last_metric: f64 = 0.0;
        let mut threshold: f64 = 0.0;
        for tok in rest.split_whitespace() {
            if let Some(v) = tok.strip_prefix("iterations_run=") {
                iterations_run = v.parse::<u32>().unwrap_or(0);
            } else if let Some(v) = tok.strip_prefix("last_metric=") {
                last_metric = v.parse::<f64>().unwrap_or(0.0);
            } else if let Some(v) = tok.strip_prefix("threshold=") {
                threshold = v.parse::<f64>().unwrap_or(0.0);
            }
        }
        return BlockerKind::IterationDidNotConverge {
            task_id: task_id.to_string(),
            iterations_run,
            last_metric,
            threshold,
        };
    }

    // Sandbox-refused marker emitted by the harness when
    // `pre_dispatch_check` rejects a task before it transitions to
    // Running. Format is `[sandbox_refused] <piece>; <piece>;...`
    // where each `<piece>` is `<KindStr>:<detail> (node=<id>)`. Empty
    // detail (`NetworkDenied:`) is allowed for unit-shaped refusals.
    // Falls back to a single Unspecified refusal when the payload
    // doesn't parse into any pieces (preserves visibility of legacy
    // free-form messages until all emission sites convert).
    if let Some(rest) = reason.strip_prefix("[sandbox_refused]") {
        let mut refusals = Vec::new();
        for piece in rest.split(';') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            let (kind, detail) = match piece.split_once(':') {
                Some((k, d)) => (k.trim().to_string(), d.trim().to_string()),
                None => (piece.to_string(), String::new()),
            };
            refusals.push(SandboxRefusalRecord { kind, detail });
        }
        if refusals.is_empty() {
            refusals.push(SandboxRefusalRecord {
                kind: "Unspecified".into(),
                detail: rest.trim().to_string(),
            });
        }
        return BlockerKind::SandboxRefused { refusals };
    }

    // Orphaned-by-crash marker produced by the dispatch WAL recovery
    // pass on harness startup.
    if let Some(rest) = reason.strip_prefix("[orphaned_by_crash]") {
        let prior_run = rest
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("prior_run="))
            .unwrap_or("unknown")
            .to_string();
        let last_dispatch_at = rest
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("at="))
            .unwrap_or("")
            .to_string();
        return BlockerKind::OrphanedByCrash {
            task_id: task_id.to_string(),
            prior_harness_run_id: prior_run,
            last_dispatch_at,
        };
    }

    // Atom-safety dispatch refusal markers. The harness
    // serialises the full typed `BlockerKind` payload as JSON after the
    // prefix so the parser round-trips every field (NetworkPolicy /
    // SandboxRequirement are typed enums whose stringification is
    // non-trivial — JSON encoding sidesteps the format design). Falls
    // back to a permissive default when the JSON is malformed so a
    // typo never strands the task without any blocker information.
    if let Some(rest) = reason.strip_prefix("[sandbox_required]") {
        let trimmed = rest.trim();
        if let Ok(parsed) = serde_json::from_str::<BlockerKind>(trimmed) {
            return parsed;
        }
        return BlockerKind::SandboxRequired {
            atom_id: "<unknown>".into(),
            requested: crate::atom::SandboxRequirement::ProcessIsolation,
            available: crate::atom::SandboxRequirement::None,
        };
    }
    if let Some(rest) = reason.strip_prefix("[network_policy_mismatch]") {
        let trimmed = rest.trim();
        if let Ok(parsed) = serde_json::from_str::<BlockerKind>(trimmed) {
            return parsed;
        }
        return BlockerKind::NetworkPolicyMismatch {
            atom_id: "<unknown>".into(),
            atom_network: crate::atom::NetworkPolicy::Bridge,
            executor_network: crate::atom::NetworkPolicy::None { allowlist: vec![] },
        };
    }
    if let Some(rest) = reason.strip_prefix("[provisioning_denied]") {
        let trimmed = rest.trim();
        if let Ok(parsed) = serde_json::from_str::<BlockerKind>(trimmed) {
            return parsed;
        }
        return BlockerKind::ProvisioningDenied {
            atom_id: "<unknown>".into(),
            package: String::new(),
            registry: String::new(),
        };
    }
    if let Some(rest) = reason.strip_prefix("[controlled_access_violation]") {
        let trimmed = rest.trim();
        if let Ok(parsed) = serde_json::from_str::<BlockerKind>(trimmed) {
            return parsed;
        }
        return BlockerKind::ControlledAccessViolation {
            task_id: task_id.to_string(),
            port_name: "<unknown>".into(),
            attempted_call: String::new(),
        };
    }

    match raw_kind {
        // Agent vocabulary includes `missing_input` for the "upstream
        // stage didn't produce the file this one needs" case. Without
        // an explicit arm this
        // fell through to `AwaitingStructuredDecision`, forcing the UI
        // into an empty decision picker even though the real remediation
        // is an upstream re-run (usually via an unapplied
        // `sme_disposition.json`). Map it to the same
        // `BlockerKind::MissingArtifact` shape the `[missing_artifact]`
        // reason-prefix path produces so `BlockerCard` renders the
        // typed summary + Rerun affordance.
        "missing_input" => {
            let missing_paths: Vec<String> = blocker_json
                .and_then(|v| v.get("missing_inputs").or_else(|| v.get("missing_paths")))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| c.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            BlockerKind::MissingArtifact {
                task_id: task_id.to_string(),
                missing_paths,
            }
        }
        // Pick-one-of-N scored candidates — the existing discovery flow.
        "awaiting_sme_selection" => {
            let (stage_id, candidates) = extract_selection_fields(blocker_json, task_id);
            BlockerKind::AwaitingSmeSelection {
                stage_id,
                candidates,
            }
        }
        // Top candidate recommended; SME confirms or overrides with
        // runner-up. Emitted by discover_batch_correction (observed).
        "awaiting_sme_approval" => {
            let (stage_id, top_candidate, runner_ups) =
                extract_approval_fields(blocker_json, task_id);
            BlockerKind::AwaitingSmeApproval {
                stage_id,
                top_candidate,
                runner_ups,
            }
        }
        // SME must authorise a same-algorithm substitute for a
        // pinned method whose runtime library isn't installed.
        "runtime_capability_missing" | "runtime_substitution" => {
            let (sme_pinned_method, missing_capability, recommended_substitute) =
                extract_runtime_substitution_fields(blocker_json, reason);
            BlockerKind::RuntimeCapabilityMissing {
                sme_pinned_method,
                missing_capability,
                recommended_substitute,
            }
        }
        // Canonical DataShapeMismatch — blocker.json carries
        // expected/actual fields the UI prefers over the reason prose.
        "data_shape_mismatch" => {
            let (expected, actual) = extract_shape_fields(blocker_json, reason);
            BlockerKind::DataShapeMismatch { expected, actual }
        }
        // Scheduler / executor said the job was killed for exceeding
        // its memory cap (SLURM `OUT_OF_MEMORY`, AWS cgroup OOM).
        "memory_exhausted" | "out_of_memory" | "oom" => {
            let peak_memory_mb = blocker_json
                .and_then(|v| v.get("peak_memory_mb"))
                .and_then(|v| v.as_u64());
            let limit_mb = blocker_json
                .and_then(|v| v.get("limit_mb"))
                .and_then(|v| v.as_u64());
            BlockerKind::MemoryExhausted {
                peak_memory_mb,
                limit_mb,
            }
        }
        // Scheduler / executor said the job exceeded its wallclock
        // cap (SLURM `TIMEOUT`, AWS RuntimeOverExpected past kill threshold).
        "time_exceeded" | "timeout" | "wallclock_exceeded" => {
            let wallclock_secs = blocker_json
                .and_then(|v| v.get("wallclock_secs"))
                .and_then(|v| v.as_u64());
            let time_limit_secs = blocker_json
                .and_then(|v| v.get("time_limit_secs"))
                .and_then(|v| v.as_u64());
            BlockerKind::TimeExceeded {
                wallclock_secs,
                time_limit_secs,
            }
        }
        // Generic "see blocker.json" bucket. The four agent-side
        // vocab strings in this arm cover "the agent has written a
        // structured decision_points_for_sme array; UI render it":
        // - awaiting_sme_input: open-ended decision list (no scored candidates)
        // - env_capability_skip: SME chooses between skipping or installing a capability
        // - (fall-through) anything we don't recognise: safest to treat as structured
        "awaiting_sme_input" | "env_capability_skip" => BlockerKind::AwaitingStructuredDecision {
            task_id: task_id.to_string(),
            decision_points_path,
            summary,
        },
        unknown => {
            // Surface future drift to operators via a one-line log
            // message. Falls through to the structured bucket — safest
            // default since it puts the decision_points_for_sme picker
            // in front of the SME.
            eprintln!(
                "[blocker_kind_unknown] task={} kind={:?} — mapping to AwaitingStructuredDecision",
                task_id, unknown
            );
            BlockerKind::AwaitingStructuredDecision {
                task_id: task_id.to_string(),
                decision_points_path,
                summary,
            }
        }
    }
}

fn one_line_summary(text: &str, max: usize) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return "Needs your input to continue.".to_string();
    }
    if first.len() <= max {
        first.to_string()
    } else {
        let truncated: String = first.chars().take(max).collect();
        format!("{}…", truncated)
    }
}

fn extract_selection_fields(
    blocker_json: Option<&serde_json::Value>,
    task_id: &str,
) -> (String, Vec<String>) {
    let stage_id = blocker_json
        .and_then(|v| v.get("stage_class").or_else(|| v.get("stage_id")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| task_id.to_string());
    let candidates = blocker_json
        .and_then(|v| v.get("candidates").or_else(|| v.get("runner_ups")))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    c.as_str()
                        .map(|s| s.to_string())
                        .or_else(|| {
                            c.get("method_id")
                                .and_then(|m| m.as_str())
                                .map(|s| s.to_string())
                        })
                        .or_else(|| c.get("id").and_then(|m| m.as_str()).map(|s| s.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    (stage_id, candidates)
}

fn extract_approval_fields(
    blocker_json: Option<&serde_json::Value>,
    task_id: &str,
) -> (String, String, Vec<String>) {
    let stage_id = blocker_json
        .and_then(|v| v.get("stage_class").or_else(|| v.get("stage_id")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| task_id.to_string());
    let top_candidate = blocker_json
        .and_then(|v| v.get("top_candidate"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let runner_ups = blocker_json
        .and_then(|v| v.get("runner_ups"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    c.as_str().map(|s| s.to_string()).or_else(|| {
                        c.get("method_id")
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string())
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (stage_id, top_candidate, runner_ups)
}

fn extract_runtime_substitution_fields(
    blocker_json: Option<&serde_json::Value>,
    reason: &str,
) -> (String, String, Option<String>) {
    let sme_pinned_method = blocker_json
        .and_then(|v| {
            v.get("sme_pinned_method")
                .or_else(|| v.get("pinned_method"))
                .or_else(|| v.get("method"))
        })
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| one_line_summary(reason, 60));
    let missing_capability = blocker_json
        .and_then(|v| {
            v.get("missing_capability")
                .or_else(|| v.get("required_capability"))
                .or_else(|| v.get("missing"))
        })
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "runtime library".to_string());
    let recommended_substitute = blocker_json
        .and_then(|v| {
            v.get("recommended_substitute")
                .or_else(|| v.get("default_if_unanswered"))
                .or_else(|| v.get("suggested_substitute"))
        })
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    (
        sme_pinned_method,
        missing_capability,
        recommended_substitute,
    )
}

fn extract_shape_fields(
    blocker_json: Option<&serde_json::Value>,
    reason: &str,
) -> (String, String) {
    let expected = blocker_json
        .and_then(|v| v.get("expected"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "see blocker detail".to_string());
    let actual = blocker_json
        .and_then(|v| v.get("actual"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| one_line_summary(reason, 160));
    (expected, actual)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct one of each variant, round-trip through JSON, assert equal.
    /// Guards against a variant being accidentally dropped from serde coverage
    /// and against the tag= shape silently drifting.
    #[test]
    fn all_variants_roundtrip_serde() {
        let variants = vec![
            BlockerKind::DataShapeMismatch {
                expected: "matrix[float64]".into(),
                actual: "list[list[int]]".into(),
            },
            BlockerKind::ValidationFailed {
                check: "cells_per_sample_min".into(),
                message: "Sample S3 has 412 cells; minimum is 500.".into(),
                cause: None,
            },
            BlockerKind::MetricBelowThreshold {
                metric: "mapping_rate".into(),
                threshold: 0.75,
                actual: 0.62,
            },
            BlockerKind::MissingInput {
                dependency: "task-42".into(),
            },
            BlockerKind::AgentError {
                message: "cellranger exited with code 137 (OOM)".into(),
            },
            BlockerKind::HostError {
                message: "could not write WORKFLOW.json: EACCES".into(),
            },
            BlockerKind::AwaitingSmeSelection {
                stage_id: "normalization".into(),
                candidates: vec!["scran".into(), "SCT".into(), "LogNormalize".into()],
            },
            BlockerKind::PilotOversize {
                projected_usd: 124.50,
                ceiling_usd: 100.00,
            },
            BlockerKind::Stalled {
                task_id: "alignment_02".into(),
                signal: StallSignalWire::CpuStarvation {
                    avg_cpu_pct: 2.1,
                    window_mins: 34,
                },
                suggested_action: StallAction::Resize,
            },
            BlockerKind::ContractViolation {
                contract_id: "best-practice-validation-contract.json".into(),
                assertion_ids: vec!["cells_per_sample_min".into(), "mapping_rate_min".into()],
            },
            BlockerKind::RuntimeCapabilityMissing {
                sme_pinned_method: "fgsea_msigdb_hallmark_reactome".into(),
                missing_capability: "r_fgsea".into(),
                recommended_substitute: Some("gseapy".into()),
            },
            BlockerKind::AwaitingStructuredDecision {
                task_id: "biological_interpretation".into(),
                decision_points_path: "runtime/outputs/biological_interpretation/blocker.json"
                    .into(),
                summary: "Runtime substitution decision needed.".into(),
            },
            BlockerKind::AwaitingSmeApproval {
                stage_id: "discover_batch_correction".into(),
                top_candidate: "harmony".into(),
                runner_ups: vec!["cca_integratelayers".into(), "scvi".into()],
            },
            BlockerKind::MissingArtifact {
                task_id: "differential_expression".into(),
                missing_paths: vec!["results/tables/de_summary.tsv".into()],
            },
            BlockerKind::HeartbeatStalled {
                task_id: "normalize_counts".into(),
                last_heartbeat_secs_ago: 1800,
            },
            BlockerKind::OrphanedByCrash {
                task_id: "alignment".into(),
                prior_harness_run_id: "8f2a1b3c".into(),
                last_dispatch_at: "2026-04-23T14:00:00Z".into(),
            },
            BlockerKind::ToolError {
                envelope: Box::new(crate::error_envelope::ToolErrorEnvelope {
                    task_id: "alignment".into(),
                    stage_id: "alignment".into(),
                    library: Some("STAR".into()),
                    library_version: Some("2.7.11a".into()),
                    error_class: "OOM".into(),
                    message: "STAR killed by SIGKILL — peak RSS 60 GiB".into(),
                    stderr_tail: vec!["killed".into()],
                    stdout_tail: vec![],
                    traceback: None,
                    exit_code: Some(137),
                    signal: Some("SIGKILL".into()),
                    wallclock_secs: Some(1800),
                    peak_memory_mb: Some(61_440),
                    input_summary: Default::default(),
                    executor: "local".into(),
                    executor_context: Default::default(),
                    captured_at: "2026-05-04T10:00:00Z".into(),
                    attempt: 1,
                    schema_version: 1,
                }),
            },
            BlockerKind::ImageDigestMismatch {
                expected_digest: "sha256:aaa".into(),
                actual_digest: "sha256:bbb".into(),
            },
            BlockerKind::ContainerPullFailed {
                image: "ghcr.io/scripps/scripps-bio-base:1.0".into(),
                reason: "unauthorized: HTTP 401".into(),
            },
            BlockerKind::ContainerStartFailed {
                image: "ghcr.io/scripps/scripps-bio-base:1.0".into(),
                reason: "nvidia driver not found".into(),
            },
            BlockerKind::RuntimeMissing {
                runtime: "apptainer".into(),
            },
            BlockerKind::SbomEmissionFailed {
                reason: "syft: unable to scan filesystem".into(),
            },
            BlockerKind::NetworkPolicyViolation {
                policy: "none".into(),
                attempted: "https://api.example.com".into(),
            },
            BlockerKind::ContainerCacheCorrupted {
                path: "~/.scripps-workflow/agent-cache/abc123".into(),
            },
            BlockerKind::MemoryExhausted {
                peak_memory_mb: Some(61_440),
                limit_mb: Some(32_768),
            },
            BlockerKind::TimeExceeded {
                wallclock_secs: Some(14_400),
                time_limit_secs: Some(14_400),
            },
            BlockerKind::ReplayCorruption {
                event_id: "evt-2026-04-23-001".into(),
                schema_version: 1,
                reason: "decision_log: missing required field `record_kind`".into(),
            },
            BlockerKind::ImageDigestUnresolved {
                image: "ghcr.io/scripps/scripps-bio-base".into(),
                tag: "1.0".into(),
                reason: "registry returned HTTP 404 for tag '1.0'".into(),
            },
            BlockerKind::CompositionInfeasible {
                missing_inputs: vec!["data:2044 (Sequence) format:1930 (FASTQ)".into()],
                unreachable_goal: Some("data:0951 / format:3475".into()),
                excluded_paths: vec![ExcludedPath {
                    atom_id: "discover_batch_correction".into(),
                    exclusion_cel: "intake.organism.taxon_id != 9606".into(),
                }],
            },
            BlockerKind::ContainerExitedAbnormally {
                exit_code: 137,
                oom_killed: true,
            },
            BlockerKind::SlurmRuntimeUnavailable {
                partition: "compute".into(),
                required: "apptainer-1.4".into(),
                available: vec!["singularity-3.x".into()],
            },
            BlockerKind::ContainerHung {
                task_id: "alignment".into(),
                container_id: "abc123".into(),
                runtime: "docker".into(),
                last_heartbeat_secs_ago: 1800,
            },
            BlockerKind::IterationDidNotConverge {
                task_id: "clustering_iter".into(),
                iterations_run: 25,
                last_metric: 0.42,
                threshold: 0.6,
            },
            BlockerKind::SandboxRefused {
                refusals: vec![SandboxRefusalRecord {
                    kind: "NetworkDenied".into(),
                    detail: "(node=t1)".into(),
                }],
            },
            BlockerKind::AdjudicationRequired {
                queue_entry_id: "adj_abc123def456".into(),
                transition_kind: "same_user_contradiction".into(),
            },
            BlockerKind::SandboxRequired {
                atom_id: "exec_atom".into(),
                requested: crate::atom::SandboxRequirement::ProcessIsolation,
                available: crate::atom::SandboxRequirement::None,
            },
            BlockerKind::NetworkPolicyMismatch {
                atom_id: "net_atom".into(),
                atom_network: crate::atom::NetworkPolicy::Bridge,
                executor_network: crate::atom::NetworkPolicy::None { allowlist: vec![] },
            },
            BlockerKind::ProvisioningDenied {
                atom_id: "compute_atom".into(),
                package: "samtools".into(),
                registry: "apt".into(),
            },
            BlockerKind::SchemaVersionMismatch {
                config_kind: "modality_config".into(),
                expected: "0.1".into(),
                found: "0.2".into(),
            },
            BlockerKind::ControlledAccessViolation {
                task_id: "alignment".into(),
                port_name: "controlled_reads".into(),
                attempted_call: "anthropic:messages:claude-sonnet-4-6".into(),
            },
            BlockerKind::OutputSizeExceeded {
                task_id: "bulk_rnaseq_alignment".into(),
                observed_bytes: 6_442_450_944,
                threshold_bytes: 5_368_709_120,
            },
            BlockerKind::PatchUnparseable {
                task_id: "compute".into(),
                rejected_path: "runtime/outputs/compute/state.patch.json.rejected-20260518T120000Z"
                    .into(),
                parse_error: "expected `:` at line 1 column 3".into(),
            },
            BlockerKind::ClockSkew {
                observed_secs: 120,
                threshold_secs: 60,
            },
            BlockerKind::WallClockExceeded {
                task_id: "alignment_01".into(),
                observed_secs: 11000,
                threshold_secs: 10800,
            },
            BlockerKind::CancelledByAmendment {
                task_id: "align_reads".into(),
                target_stage: "alignment".into(),
            },
            BlockerKind::ProvenanceCommitDropped {
                trigger: "emit".into(),
                reason: "pool_saturated".into(),
            },
            BlockerKind::TurnBudgetExceeded,
        ];

        assert_eq!(
            variants.len(),
            47,
            "expected exactly forty-seven variants \
             (was 46 pre-TurnBudgetExceeded addition; TurnBudgetExceeded \
             was added for MAX_TURNS_PER_TASK enforcement — \
             matches `BlockerKind::COUNT` compile-time gate in \
             `crates/core/tests/blocker_variant_count.rs` and the doc \
             comment on `BlockerKind` in this file)"
        );

        for v in &variants {
            let json = serde_json::to_string(v).expect("serialize");
            let back: BlockerKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(v, &back, "roundtrip mismatch for {:?}", v);
        }
    }

    /// Every `StallSignalWire` variant round-trips as `{"kind": "...",...}`.
    #[test]
    fn stall_signal_wire_roundtrips() {
        let variants = vec![
            StallSignalWire::CpuStarvation {
                avg_cpu_pct: 3.2,
                window_mins: 30,
            },
            StallSignalWire::MemoryPressure {
                pct: 94.5,
                window_mins: 5,
            },
            StallSignalWire::GpuIdleDuringTraining { window_mins: 15 },
            StallSignalWire::RuntimeOverExpected {
                actual_secs: 7200,
                expected_secs: 3600,
            },
        ];
        for s in variants {
            let json = serde_json::to_string(&s).expect("serialize");
            let back: StallSignalWire = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(s, back);
        }
    }

    /// `StallAction` serializes as snake_case scalars so the UI can
    /// treat it as a bare string.
    #[test]
    fn stall_action_serializes_as_scalar() {
        assert_eq!(
            serde_json::to_string(&StallAction::Resize).unwrap(),
            "\"resize\""
        );
        assert_eq!(
            serde_json::to_string(&StallAction::Retry).unwrap(),
            "\"retry\""
        );
        assert_eq!(
            serde_json::to_string(&StallAction::Abort).unwrap(),
            "\"abort\""
        );
        let parsed: StallAction = serde_json::from_str("\"resize\"").unwrap();
        assert_eq!(parsed, StallAction::Resize);
    }

    /// The Stalled variant's JSON has the stall signal nested, not flattened,
    /// so that the UI can pattern-match on the inner `signal.kind`.
    #[test]
    fn stalled_variant_nested_signal() {
        let b = BlockerKind::Stalled {
            task_id: "t1".into(),
            signal: StallSignalWire::MemoryPressure {
                pct: 91.3,
                window_mins: 6,
            },
            suggested_action: StallAction::Resize,
        };
        let v: serde_json::Value = serde_json::to_value(&b).unwrap();
        assert_eq!(v["kind"], "stalled");
        assert_eq!(v["task_id"], "t1");
        assert_eq!(v["signal"]["kind"], "memory_pressure");
        assert!((v["signal"]["pct"].as_f64().unwrap() - 91.3).abs() < 1e-4);
        assert_eq!(v["suggested_action"], "resize");
    }

    /// PilotOversize carries both the projection and the ceiling so the
    /// UI can render the delta without a separate metrics fetch.
    #[test]
    fn pilot_oversize_shape() {
        let b = BlockerKind::PilotOversize {
            projected_usd: 210.0,
            ceiling_usd: 150.0,
        };
        let v: serde_json::Value = serde_json::to_value(&b).unwrap();
        assert_eq!(v["kind"], "pilot_oversize");
        assert!((v["projected_usd"].as_f64().unwrap() - 210.0).abs() < 1e-6);
        assert!((v["ceiling_usd"].as_f64().unwrap() - 150.0).abs() < 1e-6);
    }

    /// Assert the internally-tagged JSON shape is stable and flat.
    #[test]
    fn deserializes_internally_tagged_json() {
        // String-only variant
        let shape_json =
            r#"{"kind":"data_shape_mismatch","expected":"matrix","actual":"data_frame"}"#;
        let parsed: BlockerKind = serde_json::from_str(shape_json).expect("parse");
        assert_eq!(
            parsed,
            BlockerKind::DataShapeMismatch {
                expected: "matrix".into(),
                actual: "data_frame".into(),
            },
        );

        // Variant carrying f64
        let metric_json = r#"{
            "kind": "metric_below_threshold",
            "metric": "qc_pass_rate",
            "threshold": 0.9,
            "actual": 0.834
        }"#;
        let parsed_metric: BlockerKind = serde_json::from_str(metric_json).expect("parse metric");
        match parsed_metric {
            BlockerKind::MetricBelowThreshold {
                metric,
                threshold,
                actual,
            } => {
                assert_eq!(metric, "qc_pass_rate");
                assert!((threshold - 0.9).abs() < f64::EPSILON);
                assert!((actual - 0.834).abs() < f64::EPSILON);
            }
            other => panic!("expected MetricBelowThreshold, got {:?}", other),
        }

        // Vec<String> variant
        let sme_json = r#"{
            "kind": "awaiting_sme_selection",
            "stage_id": "aligner",
            "candidates": ["STAR", "HISAT2", "Salmon"]
        }"#;
        let parsed_sme: BlockerKind = serde_json::from_str(sme_json).expect("parse sme");
        assert_eq!(
            parsed_sme,
            BlockerKind::AwaitingSmeSelection {
                stage_id: "aligner".into(),
                candidates: vec!["STAR".into(), "HISAT2".into(), "Salmon".into()],
            },
        );
    }

    /// BlockerContext round-trips both with and without recovery_hints.
    // ── parse_agent_blocker_kind mapper ──────────────────────────────

    #[test]
    fn mapper_awaiting_sme_selection_pulls_candidates() {
        let blocker = serde_json::json!({
            "stage_class": "clustering",
            "candidates": ["leiden", "louvain", "scanpy_leiden"],
        });
        let out = parse_agent_blocker_kind(
            "awaiting_sme_selection",
            "discover_clustering",
            "Awaiting approval",
            Some(&blocker),
        );
        match out {
            BlockerKind::AwaitingSmeSelection {
                stage_id,
                candidates,
            } => {
                assert_eq!(stage_id, "clustering");
                assert_eq!(candidates, vec!["leiden", "louvain", "scanpy_leiden"]);
            }
            _ => panic!("expected AwaitingSmeSelection, got {:?}", out),
        }
    }

    #[test]
    fn mapper_awaiting_sme_approval_pulls_top_and_runner_ups() {
        let blocker = serde_json::json!({
            "stage_class": "batch_correction",
            "top_candidate": "harmony",
            "runner_ups": [
                {"method_id": "cca_integratelayers", "score": 0.576},
                {"method_id": "scvi", "score": 0.538},
            ],
        });
        let out = parse_agent_blocker_kind(
            "awaiting_sme_approval",
            "discover_batch_correction",
            "…",
            Some(&blocker),
        );
        match out {
            BlockerKind::AwaitingSmeApproval {
                stage_id,
                top_candidate,
                runner_ups,
            } => {
                assert_eq!(stage_id, "batch_correction");
                assert_eq!(top_candidate, "harmony");
                assert_eq!(runner_ups, vec!["cca_integratelayers", "scvi"]);
            }
            _ => panic!("expected AwaitingSmeApproval, got {:?}", out),
        }
    }

    #[test]
    fn mapper_runtime_capability_missing_pulls_method_and_substitute() {
        let blocker = serde_json::json!({
            "sme_pinned_method": "pseudobulk_deseq2",
            "missing_capability": "r_deseq2",
            "recommended_substitute": "pydeseq2",
        });
        let out = parse_agent_blocker_kind(
            "runtime_capability_missing",
            "differential_expression",
            "R DESeq2 not installed",
            Some(&blocker),
        );
        match out {
            BlockerKind::RuntimeCapabilityMissing {
                sme_pinned_method,
                missing_capability,
                recommended_substitute,
            } => {
                assert_eq!(sme_pinned_method, "pseudobulk_deseq2");
                assert_eq!(missing_capability, "r_deseq2");
                assert_eq!(recommended_substitute, Some("pydeseq2".into()));
            }
            _ => panic!("expected RuntimeCapabilityMissing, got {:?}", out),
        }
    }

    #[test]
    fn mapper_runtime_substitution_alias_maps_to_same_variant() {
        let blocker = serde_json::json!({
            "sme_pinned_method": "fgsea_msigdb_hallmark_reactome",
            "missing_capability": "r_fgsea",
        });
        let out = parse_agent_blocker_kind(
            "runtime_substitution",
            "biological_interpretation",
            "R fgsea missing",
            Some(&blocker),
        );
        assert!(matches!(out, BlockerKind::RuntimeCapabilityMissing { .. }));
    }

    #[test]
    fn mapper_data_shape_mismatch_pulls_expected_actual() {
        let blocker = serde_json::json!({
            "expected": "matrix[float64]",
            "actual": "list[list[int]]",
        });
        let out = parse_agent_blocker_kind(
            "data_shape_mismatch",
            "preprocessing",
            "ignored",
            Some(&blocker),
        );
        match out {
            BlockerKind::DataShapeMismatch { expected, actual } => {
                assert_eq!(expected, "matrix[float64]");
                assert_eq!(actual, "list[list[int]]");
            }
            _ => panic!("expected DataShapeMismatch, got {:?}", out),
        }
    }

    #[test]
    fn mapper_awaiting_sme_input_maps_to_structured_decision() {
        let out = parse_agent_blocker_kind(
            "awaiting_sme_input",
            "data_acquisition",
            "SME must pick a cohort subset.",
            None,
        );
        match out {
            BlockerKind::AwaitingStructuredDecision {
                task_id,
                decision_points_path,
                summary,
            } => {
                assert_eq!(task_id, "data_acquisition");
                assert_eq!(
                    decision_points_path,
                    "runtime/outputs/data_acquisition/blocker.json"
                );
                assert_eq!(summary, "SME must pick a cohort subset.");
            }
            _ => panic!("expected AwaitingStructuredDecision, got {:?}", out),
        }
    }

    #[test]
    fn mapper_env_capability_skip_maps_to_structured_decision() {
        let out = parse_agent_blocker_kind(
            "env_capability_skip",
            "discover_normalization",
            "capability check failed",
            None,
        );
        assert!(matches!(
            out,
            BlockerKind::AwaitingStructuredDecision { .. }
        ));
    }

    #[test]
    fn mapper_unknown_string_falls_through_with_stderr_warning() {
        // Can't easily capture stderr in a Rust unit test without
        // extra machinery, but we can at least confirm the variant
        // is the safe structured bucket — the eprintln stays in the
        // source for operators to grep.
        let out = parse_agent_blocker_kind(
            "some_future_kind_the_agent_invented",
            "some_task",
            "Reason goes here.",
            None,
        );
        assert!(matches!(
            out,
            BlockerKind::AwaitingStructuredDecision { .. }
        ));
    }

    #[test]
    fn mapper_missing_input_kind_pulls_paths_from_json() {
        // `missing_input` maps directly to MissingArtifact instead of
        // falling through to the unknown-arm AwaitingStructuredDecision.
        let blocker = serde_json::json!({
            "blocker_kind": "missing_input",
            "missing_inputs": [
                "runtime/outputs/integration/integrated.h5ad",
                "runtime/outputs/integration/integrated_summary.json",
            ],
        });
        let out = parse_agent_blocker_kind(
            "missing_input",
            "trajectory_analysis",
            "Upstream integration output is missing.",
            Some(&blocker),
        );
        match out {
            BlockerKind::MissingArtifact {
                task_id,
                missing_paths,
            } => {
                assert_eq!(task_id, "trajectory_analysis");
                assert_eq!(
                    missing_paths,
                    vec![
                        "runtime/outputs/integration/integrated.h5ad".to_string(),
                        "runtime/outputs/integration/integrated_summary.json".to_string(),
                    ]
                );
            }
            other => panic!("expected MissingArtifact, got {:?}", other),
        }
    }

    #[test]
    fn mapper_missing_input_kind_without_json_returns_empty_paths() {
        let out = parse_agent_blocker_kind(
            "missing_input",
            "trajectory_analysis",
            "Upstream output is missing.",
            None,
        );
        match out {
            BlockerKind::MissingArtifact {
                task_id,
                missing_paths,
            } => {
                assert_eq!(task_id, "trajectory_analysis");
                assert!(missing_paths.is_empty());
            }
            other => panic!("expected MissingArtifact, got {:?}", other),
        }
    }

    #[test]
    fn mapper_container_hung_prefix_pulls_id_runtime_age() {
        let out = parse_agent_blocker_kind(
            "",
            "alignment",
            "[container_hung] task=alignment age_secs=1800 container_id=abc123 runtime=docker — heartbeat stale but container still alive (threshold 900s).",
            None,
        );
        match out {
            BlockerKind::ContainerHung {
                task_id,
                container_id,
                runtime,
                last_heartbeat_secs_ago,
            } => {
                assert_eq!(task_id, "alignment");
                assert_eq!(container_id, "abc123");
                assert_eq!(runtime, "docker");
                assert_eq!(last_heartbeat_secs_ago, 1800);
            }
            other => panic!("expected ContainerHung, got {:?}", other),
        }
    }

    #[test]
    fn mapper_container_hung_apptainer_runtime_with_empty_id() {
        // apptainer instance list returns the instance name in `live`,
        // not a container id; the parser preserves whatever the marker
        // emitted. Empty container_id is valid for apptainer.
        let out = parse_agent_blocker_kind(
            "",
            "qc_preprocessing",
            "[container_hung] task=qc_preprocessing age_secs=2400 container_id= runtime=apptainer — heartbeat stale.",
            None,
        );
        match out {
            BlockerKind::ContainerHung {
                runtime,
                container_id,
                last_heartbeat_secs_ago,
                ..
            } => {
                assert_eq!(runtime, "apptainer");
                assert_eq!(container_id, "");
                assert_eq!(last_heartbeat_secs_ago, 2400);
            }
            other => panic!("expected ContainerHung, got {:?}", other),
        }
    }

    #[test]
    fn mapper_heartbeat_stalled_path_unchanged_when_no_container_signal() {
        // No `[container_hung]` prefix → falls through to the legacy
        // HeartbeatStalled variant. Guards against the new prefix
        // arm shadowing the existing one.
        let out = parse_agent_blocker_kind(
            "",
            "trajectory_analysis",
            "[heartbeat_stalled] task=trajectory_analysis age_secs=1800 — no heartbeat update in 1800s (threshold 900s).",
            None,
        );
        match out {
            BlockerKind::HeartbeatStalled {
                task_id,
                last_heartbeat_secs_ago,
            } => {
                assert_eq!(task_id, "trajectory_analysis");
                assert_eq!(last_heartbeat_secs_ago, 1800);
            }
            other => panic!("expected HeartbeatStalled, got {:?}", other),
        }
    }

    #[test]
    fn mapper_missing_artifact_prefix_upgrades() {
        let out = parse_agent_blocker_kind(
            "",
            "differential_expression",
            "[missing_artifact] task=differential_expression paths=results/tables/de_summary.tsv,reports/de.md — agent marked completed but required artifacts are missing or empty.",
            None,
        );
        match out {
            BlockerKind::MissingArtifact {
                task_id,
                missing_paths,
            } => {
                assert_eq!(task_id, "differential_expression");
                assert_eq!(
                    missing_paths,
                    vec![
                        "results/tables/de_summary.tsv".to_string(),
                        "reports/de.md".to_string(),
                    ]
                );
            }
            other => panic!("expected MissingArtifact, got {:?}", other),
        }
    }

    #[test]
    fn mapper_with_envelope_always_promotes_to_tool_error() {
        let env = crate::error_envelope::ToolErrorEnvelope {
            task_id: "alignment".into(),
            stage_id: "alignment".into(),
            library: Some("STAR".into()),
            library_version: None,
            error_class: "OOM".into(),
            message: "killed".into(),
            stderr_tail: vec![],
            stdout_tail: vec![],
            traceback: None,
            exit_code: Some(137),
            signal: Some("SIGKILL".into()),
            wallclock_secs: None,
            peak_memory_mb: None,
            input_summary: Default::default(),
            executor: "local".into(),
            executor_context: Default::default(),
            captured_at: "2026-05-04T10:00:00Z".into(),
            attempt: 1,
            schema_version: 1,
        };
        let out = parse_agent_blocker_kind_with_envelope(
            "data_shape_mismatch",
            "alignment",
            "ignored",
            None,
            Some(env.clone()),
        );
        match out {
            BlockerKind::ToolError { envelope } => {
                assert_eq!(envelope.error_class, "OOM");
                assert_eq!(envelope.task_id, "alignment");
            }
            other => panic!("expected ToolError, got {:?}", other),
        }
    }

    #[test]
    fn mapper_without_envelope_unchanged_behavior() {
        let out = parse_agent_blocker_kind_with_envelope(
            "awaiting_sme_input",
            "t1",
            "Need input.",
            None,
            None,
        );
        assert!(matches!(
            out,
            BlockerKind::AwaitingStructuredDecision { .. }
        ));
    }

    #[test]
    fn mapper_truncates_very_long_summary() {
        let long = "x".repeat(500);
        let out = parse_agent_blocker_kind("awaiting_sme_input", "t", &long, None);
        match out {
            BlockerKind::AwaitingStructuredDecision { summary, .. } => {
                assert!(summary.len() < 200, "got {} chars", summary.len());
                assert!(summary.ends_with('…'));
            }
            _ => panic!("expected AwaitingStructuredDecision"),
        }
    }

    #[test]
    fn map_sandbox_refused_blocker_reason() {
        let reason = "[sandbox_refused] task=t1 — NetworkDenied";
        let kind = parse_agent_blocker_kind("", "t1", reason, None);
        match kind {
            BlockerKind::SandboxRefused { refusals } => {
                assert!(!refusals.is_empty());
            }
            other => panic!("expected SandboxRefused, got {other:?}"),
        }
    }

    #[test]
    fn context_serde_roundtrip() {
        let with_hints = BlockerContext {
            timestamp: "2026-04-16T12:34:56Z".into(),
            recovery_hints: Some("Re-run stage with `--force` to clear the stale lock.".into()),
        };
        let json = serde_json::to_string(&with_hints).expect("serialize with hints");
        let back: BlockerContext = serde_json::from_str(&json).expect("deserialize with hints");
        assert_eq!(with_hints, back);

        let without_hints = BlockerContext {
            timestamp: "2026-04-16T00:00:00Z".into(),
            recovery_hints: None,
        };
        let json2 = serde_json::to_string(&without_hints).expect("serialize without hints");
        let back2: BlockerContext =
            serde_json::from_str(&json2).expect("deserialize without hints");
        assert_eq!(without_hints, back2);
    }

    #[test]
    fn validation_failed_carries_optional_literature_cause() {
        let b = BlockerKind::ValidationFailed {
            check: "evidence_quote_substring_match".into(),
            message: "row 4: evidence_quote not in source".into(),
            cause: Some(ValidationFailureCause::LiteratureClaim {
                row_index: 4,
                artifact: "prior_claims_matrix.csv".into(),
                kind: LiteratureClaimFailureKind::QuoteNotInSource,
            }),
        };
        match b {
            BlockerKind::ValidationFailed {
                cause:
                    Some(ValidationFailureCause::LiteratureClaim {
                        row_index, kind, ..
                    }),
                ..
            } => {
                assert_eq!(row_index, 4);
                assert!(matches!(kind, LiteratureClaimFailureKind::QuoteNotInSource));
            }
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn validation_failed_without_cause_still_constructs() {
        let _b = BlockerKind::ValidationFailed {
            check: "p_value_in_unit_interval".into(),
            message: "got p=1.5".into(),
            cause: None,
        };
    }

    #[test]
    fn sandbox_required_blocker_displays() {
        let b = BlockerKind::SandboxRequired {
            atom_id: "exec_atom".into(),
            requested: crate::atom::SandboxRequirement::ProcessIsolation,
            available: crate::atom::SandboxRequirement::None,
        };
        let s = format!("{b:?}");
        assert!(s.contains("SandboxRequired"));
    }

    #[test]
    fn network_policy_mismatch_blocker_displays() {
        let b = BlockerKind::NetworkPolicyMismatch {
            atom_id: "net_atom".into(),
            atom_network: crate::atom::NetworkPolicy::Bridge,
            executor_network: crate::atom::NetworkPolicy::None { allowlist: vec![] },
        };
        assert!(format!("{b:?}").contains("NetworkPolicyMismatch"));
    }

    #[test]
    fn provisioning_denied_blocker_displays() {
        let b = BlockerKind::ProvisioningDenied {
            atom_id: "compute_atom".into(),
            package: "samtools".into(),
            registry: "apt".into(),
        };
        assert!(format!("{b:?}").contains("ProvisioningDenied"));
    }
}
