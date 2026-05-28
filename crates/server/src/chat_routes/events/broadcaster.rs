//! Broadcaster-side helpers for the events domain:
//! synthesize-on-the-fly stub `decision.json` writer + typed
//! `BlockerKind` parsers consumed by the progress handler before it
//! issues an SSE broadcast.
//!
//! These live alongside (not inside) the `BroadcastEventSink` bridge in
//! `chat_routes/event_sink.rs`. The sink is the runtime fanout
//! mechanism; this file owns the deterministic helpers that decide
//! WHAT gets fanned out (typed BlockerKind, stubbed decision.json) for
//! the harness-progress write path.

/// best-effort synthesis of a stub `decision.json` when a
/// discovery blocker's reason references the path but the agent didn't
/// actually write the file. Keeps the `BlockerCard` rich-radio picker
/// reachable on ~50 %-compliance agent output: the UI's fetch resolves
/// with a single-candidate stub instead of 404'ing into the plain-text
/// degrade path.
///
/// Expected reason shape (from `scripts/agent-claude.sh` default
/// discovery block):
///
/// ```text
/// Awaiting SME approval for normalization. Top candidate: vst
/// (score 0.91). Runner-ups: tmm (0.82), cpm (0.71). Rationale:
/// best-practice scorer pick. Full decision:
/// runtime/outputs/discover_normalization/decision.json
/// ```
///
/// Falls back to a 1-field stub when no structured subfields parse.
pub(super) fn synthesize_missing_decision_json(package_root: &std::path::Path, detail: &str) {
    // 1. Is the path referenced at all? The first character must be
    // alphanumeric or underscore so `.` / `..` (and `..-.foo` etc.)
    // cannot match.
    let path_re =
        match regex::Regex::new(r"runtime/outputs/([A-Za-z0-9_][A-Za-z0-9_.\-]*)/decision\.json") {
            Ok(r) => r,
            Err(_) => return,
        };
    let caps = match path_re.captures(detail) {
        Some(c) => c,
        None => return,
    };
    let task_id = caps
        .get(1)
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    if task_id.is_empty() {
        return;
    }
    // Belt-and-suspenders: even though the regex now blocks `.` / `..`,
    // route through the path-jail helper so a future regex tweak can't
    // re-introduce traversal. If the helper rejects, abort the synth.
    let task_dir = match super::super::_path_jail::runtime_outputs_for_task(package_root, &task_id)
    {
        Ok(p) => p,
        Err(_) => return,
    };
    let decision_path = task_dir.join("decision.json");
    if decision_path.exists() {
        return;
    }

    // 2. Parse structured subfields from the reason.
    let top = regex::Regex::new(r"(?i)top candidate:\s*([A-Za-z0-9_.\-]+)")
        .ok()
        .and_then(|r| r.captures(detail))
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()));
    // Match up to the next newline OR the start of a known neighbor
    // field (Rationale: / Full decision:) so embedded parenthesized
    // numbers like `(0.82)` don't truncate the capture.
    let runner_ups: Vec<String> = regex::Regex::new(r"(?i)runner-?ups?:\s*([^\n]+)")
        .ok()
        .and_then(|r| r.captures(detail))
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        .map(|s| {
            let s = s
                .split("Rationale")
                .next()
                .unwrap_or(&s)
                .split("Full decision")
                .next()
                .unwrap_or(&s)
                .to_string();
            s.split(',')
                .map(|t| t.split_whitespace().next().unwrap_or_default().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let rationale = regex::Regex::new(r"(?i)rationale:\s*([^.\n]+)")
        .ok()
        .and_then(|r| r.captures(detail))
        .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()));

    // 3. Build the stub.
    let stub = match top {
        Some(top_candidate) => serde_json::json!({
            "task_id": task_id,
            "top_candidate": top_candidate,
            "runner_ups": runner_ups,
            "rationale": rationale,
            "auto_picked": false,
            "stub": true,
            "note": "agent did not write decision.json — stub synthesized by server"
        }),
        None => serde_json::json!({
            "task_id": task_id,
            "top_candidate": "unknown",
            "auto_picked": false,
            "stub": true,
            "note": "agent did not write decision.json — stub synthesized by server"
        }),
    };

    // 4. Write, creating the parent dir. Best-effort; failures are
    // silently swallowed — the UI's 404 fallback still fires.
    if let Some(parent) = decision_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &decision_path,
        serde_json::to_vec_pretty(&stub).unwrap_or_default(),
    );
}

/// Best-effort parse of a harness-reported blocker detail into a typed
/// BlockerKind. The mock agent emits JSON-shaped detail so the UI gets
/// a precise recovery affordance; production agents that emit plain
/// strings fall through to DataShapeMismatch (the most common case).
fn parse_harness_blocker_kind(detail: &str) -> ecaa_workflow_core::blocker::BlockerKind {
    use ecaa_workflow_core::blocker::BlockerKind;
    // Case 1 (legacy happy path): detail is a serialized BlockerKind
    // JSON — deserialize directly.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(detail) {
        if let Ok(parsed) = serde_json::from_value::<BlockerKind>(v) {
            return parsed;
        }
    }
    // Case 2: detail is the agent's prose reason — fall back to a
    // conservative DataShapeMismatch so the UI at least renders
    // something readable. The richer typed resolution happens in
    // `parse_harness_blocker_kind_with_file` below, called by the
    // progress handler once it knows the package root.
    BlockerKind::DataShapeMismatch {
        expected: "see blocker detail".into(),
        actual: if detail.is_empty() {
            "unspecified".into()
        } else {
            detail.into()
        },
    }
}

/// package-aware blocker-kind resolver. Reads the
/// agent-written `runtime/outputs/<task>/blocker.json` (the
/// authoritative source of `blocker_kind` + structured decision
/// points) and runs it through
/// `ecaa_workflow_core::blocker::parse_agent_blocker_kind` so the
/// UI gets a typed variant instead of the lossy `DataShapeMismatch`
/// fallback that collapsed every `runtime_substitution` /
/// `awaiting_sme_input` blocker to "The data doesn't match the
/// expected shape". Falls back to `parse_harness_blocker_kind` on
/// missing / unparseable file so existing callers keep working.
pub(super) fn parse_harness_blocker_kind_with_file(
    detail: &str,
    package_dir: Option<&std::path::Path>,
    task_id: &str,
) -> ecaa_workflow_core::blocker::BlockerKind {
    use ecaa_workflow_core::blocker::{
        parse_agent_blocker_kind, parse_agent_blocker_kind_with_envelope, BlockerKind,
    };
    use ecaa_workflow_core::error_envelope::ToolErrorEnvelope;

    // The TypedBlockers ablation (ECAA_ABLATE_TYPED_BLOCKERS) is
    // emit-only: suppression happens in
    // `conversation::emit::sidecars::write_typed_blocker` so the
    // package's `runtime/typed-blocker.json` sidecar is absent on the
    // ablated arm, while the live SSE path always returns typed blockers.
    // The broadcaster never checks the flag — Arm B′ packages have no
    // typed-blocker sidecar, which is the construct-validity surface the
    // eval-adapters tier-21 corpus tests assert on.

    // First check for a structured tool-error envelope at
    // `runtime/outputs/<task>/error.json`. When present it takes
    // precedence over every other shape — captures from any backend
    // (local stderr, AWS SSM, SLURM sacct) all land in the same
    // schema so this branch surfaces the typed `BlockerKind::ToolError`
    // variant the BlockerCard's tool_error arm renders.
    if let Some(pkg) = package_dir {
        // Route task-keyed paths through the path-jail even though
        // `task_id` here comes from harness-side accounting (not URL):
        // defense-in-depth ensures a corrupted progress event can't
        // induce a read outside the package root.
        if let Ok(task_dir) = super::super::_path_jail::runtime_outputs_for_task(pkg, task_id) {
            let env_path = task_dir.join("error.json");
            if let Ok(bytes) = std::fs::read(&env_path) {
                if let Ok(env) = serde_json::from_slice::<ToolErrorEnvelope>(&bytes) {
                    return parse_agent_blocker_kind_with_envelope(
                        "",
                        task_id,
                        detail,
                        None,
                        Some(env),
                    );
                }
            }

            let path = task_dir.join("blocker.json");
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(blocker_json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    let raw_kind = blocker_json
                        .get("blocker_kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("awaiting_sme_input");
                    return parse_agent_blocker_kind(
                        raw_kind,
                        task_id,
                        detail,
                        Some(&blocker_json),
                    );
                }
            }
        }
    }
    // No file or unparseable — try the legacy path that accepts a
    // direct BlockerKind JSON blob in `detail`, else DataShapeMismatch.
    let fallback = parse_harness_blocker_kind(detail);
    // If the legacy fallback produced the generic DataShapeMismatch,
    // upgrade to AwaitingStructuredDecision so the UI at least knows
    // to try fetching blocker.json when it renders the card. Preserves
    // all other typed results (Stalled from stall monitor, etc.).
    match fallback {
        BlockerKind::DataShapeMismatch { expected, actual } if expected == "see blocker detail" => {
            parse_agent_blocker_kind("awaiting_sme_input", task_id, &actual, None)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The regex rejects leading `.` so traversal-shaped task_ids never
    // match; combined with the path-jail wrapper above, the synthesize
    // function refuses to write outside the package root.
    #[test]
    fn synthesize_decision_json_rejects_dotdot_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit")).unwrap();
        // The detail string references a traversal task_id; the regex
        // must NOT match it, so no file is written.
        let detail = "Full decision: runtime/outputs/../decision.json";
        synthesize_missing_decision_json(pkg, detail);
        // Confirm no file appeared at the traversal location.
        assert!(!tmp.path().join("decision.json").exists());
        assert!(!pkg.join("runtime/decision.json").exists());
    }

    #[test]
    fn synthesize_decision_json_rejects_dotprefix_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs")).unwrap();
        // A `.foo`-shaped task_id (legal characters but starts with dot)
        // is now blocked by the leading-character class.
        let detail = "Full decision: runtime/outputs/.hidden/decision.json";
        synthesize_missing_decision_json(pkg, detail);
        assert!(!pkg.join("runtime/outputs/.hidden/decision.json").exists());
    }

    #[test]
    fn synthesize_decision_json_writes_for_legit_task() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/discover_normalization")).unwrap();
        let detail = "Top candidate: vst (score 0.91). Rationale: best. Full decision: \
                      runtime/outputs/discover_normalization/decision.json";
        synthesize_missing_decision_json(pkg, detail);
        let expected = pkg.join("runtime/outputs/discover_normalization/decision.json");
        assert!(expected.exists(), "expected stub at {}", expected.display());
    }
}
