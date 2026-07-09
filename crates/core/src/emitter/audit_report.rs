//! Human-readable Markdown summary of a package's ECAA / provenance /
//! auditability JSON sidecars, written into the deposit as `AUDIT-REPORT.md`.
//!
//! The science is already narrated by `final_report.md` and the execution
//! order by `EXECUTION-ORDER.md`; this report surfaces the *audit + provenance*
//! layer that otherwise lives only as JSON/JSONL: the audit-proof invariants,
//! the claim-verification verdicts, the method decisions, assumptions, typed
//! data-flow proofs, validation obligations, re-execution equivalence, the
//! repair pass, the cost ledger, and catalog coverage.
//!
//! It is a *derived view*: every value is read from a sidecar under
//! `runtime/`; no number is invented. Each section degrades gracefully when its
//! sidecar is absent. Determinism: the report carries no fresh wall-clock — it
//! reuses the `evaluated_at` already recorded in `audit-proof-report.json` — so
//! it is a pure function of the package's sidecars.

use anyhow::{Context, Result};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::Path;

const NULL: Value = Value::Null;

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

fn read_jsonl(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p)
        .map(|t| {
            t.lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Field accessor that never panics.
fn g<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&NULL)
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// Sanitize a string for a Markdown table cell (no pipes / newlines) and clamp.
fn cell(v: &str, max: usize) -> String {
    let one_line = v.replace(['\n', '\r'], " ").replace('|', "\\|");
    let t = one_line.trim();
    if t.chars().count() > max {
        let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        t.to_string()
    }
}

/// Where a reviewer finds the full record for a routed-to-review `failure` and
/// how to act on it. The report cell clamps `detail` to keep the table
/// readable; this points at the authoritative sidecar (which carries the
/// untruncated list), the UI surface, and — for the two offline-operator
/// invariants — the exact command that clears the "unverified" state. Keyed on
/// the failure `source` (`"ClaimMismatch"` or `{"InvariantFailure": "<name>"}`).
fn review_locus(f: &Value) -> String {
    if let Some(name) = g(f, "source").get("InvariantFailure").and_then(Value::as_str) {
        return match name {
            "equivalence_failure" => {
                "offline step — run `ecaa-workflow replay --tier all <package>` to re-execute \
                 the recorded compute and populate re-execution equivalence"
                    .to_string()
            }
            "substrate_validity" => {
                "offline step — install runcrate (≥0.5) then `ecaa-workflow replay --tier \
                 verify <package>` to record WRROC substrate validity"
                    .to_string()
            }
            "evidence_coverage" => {
                "runtime/audit-proof-report.json → verdicts[evidence_coverage].detail (full \
                 unreferenced-output list); Repairs + Documents tabs"
                    .to_string()
            }
            "claim_completeness" => {
                "runtime/audit-proof-report.json → verdicts[claim_completeness].detail + \
                 runtime/claim-verification.json (empty-supported_by claims); Claims tab"
                    .to_string()
            }
            "cross_graph_integrity" => {
                "runtime/audit-proof-report.json → verdicts[cross_graph_integrity].detail \
                 (dangling @ids); cross-check ro-crate-metadata.json"
                    .to_string()
            }
            other => format!("runtime/audit-proof-report.json → verdicts[{other}].detail"),
        };
    }
    if g(f, "source").as_str() == Some("ClaimMismatch") {
        let task = s(g(f, "task"));
        return format!(
            "runtime/claim-verification.json (claim {}); narrative runtime/outputs/{}/; Claims tab",
            s(g(f, "id")),
            task
        );
    }
    format!("runtime/repair-status.json (failure {})", s(g(f, "id")))
}

/// The audit-proof invariant id a routed-to-review `failure` maps to, if any.
/// Audit-invariant failures carry `source: {"InvariantFailure": "<id>"}` (or an
/// `audit` task whose `subject` names the invariant); other failures (e.g.
/// `ClaimMismatch`) map to no invariant and return `None`.
fn review_invariant_id(f: &Value) -> Option<&str> {
    if let Some(inv) = f
        .get("source")
        .and_then(|src| src.get("InvariantFailure"))
        .and_then(Value::as_str)
    {
        return Some(inv);
    }
    if s(g(f, "task")) == "audit" {
        let subject = s(g(f, "subject"));
        if !subject.is_empty() {
            return Some(subject);
        }
    }
    None
}

/// Current status (`pass`/`warn`/`fail`/`unverified`) of an audit-proof
/// invariant, read from the (post-reseal) `audit-proof-report.json` verdicts.
/// This is what lets the historical repair-pass snapshot reconcile against the
/// live invariant table: an offline step the pass routed for later (e.g.
/// `equivalence_failure` / `substrate_validity`) shows its cleared status once a
/// subsequent `replay` / `reexec --reseal` has run.
fn invariant_status<'a>(ap: &'a Value, id: &str) -> Option<&'a str> {
    let st = ap
        .get("verdicts")
        .and_then(Value::as_array)?
        .iter()
        .find(|v| s(g(v, "id")) == id)
        .map(|v| s(g(v, "status")))?;
    (!st.is_empty()).then_some(st)
}

/// Write `dst/AUDIT-REPORT.md` from the package's audit/provenance sidecars.
/// Best-effort per section; returns `Err` only if the file cannot be written.
pub(super) fn write_audit_report(dst: &Path) -> Result<()> {
    let rt = dst.join("runtime");
    let mut m = String::new();

    // ── Header / identity ────────────────────────────────────────────────
    let ap = read_json(&rt.join("audit-proof-report.json")).unwrap_or(Value::Null);
    let _ = writeln!(m, "# Audit & provenance report\n");
    let _ = writeln!(
        m,
        "Human-readable summary of this package's ECAA accountability and \
         provenance artifacts, generated from the JSON sidecars under \
         `runtime/`. The scientific findings are in `final_report.md`; this \
         report covers what those reports do not — the claim-verification \
         verdicts, audit-proof invariants, decisions, assumptions, typed \
         data-flow proofs, validation obligations, re-execution equivalence, \
         the repair pass, cost, and coverage. Every value here is read from a \
         sidecar; nothing is asserted from prose.\n",
    );
    let _ = writeln!(m, "| Field | Value |");
    let _ = writeln!(m, "| :---- | :---- |");
    let iri = s(g(&ap, "package_iri"));
    if !iri.is_empty() {
        let _ = writeln!(m, "| Package IRI | `{}` |", cell(iri, 120));
    }
    let _ = writeln!(m, "| ECAA spec version | {} |", cell(s(g(&ap, "ecaa_version")), 40));
    let _ = writeln!(
        m,
        "| Audit evaluated at | {} |",
        cell(s(g(&ap, "evaluated_at")), 40)
    );
    if let Some(ev) = ap.get("evaluator") {
        let policy = s(g(ev, "policy"));
        let _ = writeln!(
            m,
            "| Evaluator | {} {}{} |",
            cell(s(g(ev, "impl")), 40),
            cell(s(g(ev, "version")), 20),
            if policy.is_empty() {
                String::new()
            } else {
                format!(" ({})", cell(policy, 20))
            },
        );
    }
    let _ = writeln!(m);

    // ── Audit-proof invariants ───────────────────────────────────────────
    if let Some(verdicts) = ap.get("verdicts").and_then(Value::as_array) {
        let _ = writeln!(m, "## Audit-proof invariants\n");
        let _ = writeln!(
            m,
            "Deterministic integrity checks over the package graph (warn-only \
             mode: warnings/failures are surfaced, not blocking).\n"
        );
        let _ = writeln!(m, "| Invariant | Status | Violations / inspected | Detail |");
        let _ = writeln!(m, "| :---- | :---- | :---- | :---- |");
        for v in verdicts {
            let _ = writeln!(
                m,
                "| {} | **{}** | {} / {} | {} |",
                cell(s(g(v, "id")), 40),
                cell(s(g(v, "status")), 12),
                g(v, "n_violations").as_u64().unwrap_or(0),
                g(v, "n_inspected").as_u64().unwrap_or(0),
                cell(s(g(v, "detail")), 160),
            );
        }
        let _ = writeln!(m);
        let _ = writeln!(
            m,
            "_Detail is clamped above; the full per-invariant violation list is in \
             `runtime/audit-proof-report.json` (`verdicts[].detail`). Review `warn`/`fail` \
             invariants in the UI Repairs, Claims, and Reproducibility tabs. An `unverified` \
             status is an offline operator step, not a failure: `equivalence_failure` clears \
             after `ecaa-workflow replay --tier all <package>`, and `substrate_validity` after \
             installing runcrate and running `ecaa-workflow replay --tier verify <package>`._\n"
        );
    }

    // ── Claim verification ───────────────────────────────────────────────
    if let Some(cv) = read_json(&rt.join("claim-verification.json")) {
        let _ = writeln!(m, "## Claim verification (narrative ↔ result tables)\n");
        let n = |k: &str| g(&cv, k).as_u64().unwrap_or(0);
        let _ = writeln!(
            m,
            "Each candidate claim extracted from the task narratives is tested \
             against the result tables. **{} checked → {} verified, {} mismatch, \
             {} suspicious, {} unverifiable/pending.** A claim is *Verified* only \
             when it points at a comparable number in a result table; the rest \
             are literature-direction, interpretation, and stage-summary \
             statements with no single comparable cell.\n",
            n("n_checked"),
            n("n_verified"),
            n("n_mismatch"),
            n("n_suspicious"),
            n("n_unverifiable"),
        );
        if let Some(arr) = cv.get("verdicts").and_then(Value::as_array) {
            // Mismatches + suspicious first (the integrity-critical ones), then
            // a sample of verified; pending are summarized, not enumerated.
            let mut shown = 0usize;
            let _ = writeln!(m, "| Claim | Entity | Status | Claim text |");
            let _ = writeln!(m, "| :---- | :---- | :---- | :---- |");
            for want in ["mismatch", "suspicious", "verified"] {
                for v in arr.iter().filter(|v| s(g(v, "status")) == want) {
                    if want == "verified" && shown >= 40 {
                        break;
                    }
                    let _ = writeln!(
                        m,
                        "| {} | {} | {} | {} |",
                        cell(s(g(v, "claim_id")), 48),
                        cell(s(g(v, "entity")), 24),
                        cell(s(g(v, "status")), 12),
                        cell(s(g(v, "text")), 110),
                    );
                    shown += 1;
                }
            }
            let pending = arr.iter().filter(|v| s(g(v, "status")) == "pending").count();
            if pending > 0 {
                let _ = writeln!(
                    m,
                    "\n_{} pending (literature-direction / interpretation / \
                     stage-summary) claims not enumerated; see \
                     `runtime/claim-verification.json`._",
                    pending
                );
            }
        }
        let _ = writeln!(m);
    }

    // ── Method decisions & intent ────────────────────────────────────────
    let decisions = read_jsonl(&rt.join("decisions.jsonl"));
    if !decisions.is_empty() {
        let _ = writeln!(m, "## Method decisions & recorded intent\n");
        let _ = writeln!(
            m,
            "{} decision(s) recorded, each with an authority. Method choices at \
             discovery stages carry the selected method + rationale; the intake \
             intent is recorded as the originating request.\n",
            decisions.len()
        );
        let _ = writeln!(m, "| Timestamp | Kind | Actor | Authority | Summary |");
        let _ = writeln!(m, "| :---- | :---- | :---- | :---- | :---- |");
        for d in &decisions {
            let dec = g(d, "decision");
            // The `decision` payload is a tagged object; surface its kind + a
            // brief from whichever descriptive field it carries.
            let kind = s(g(dec, "kind"));
            // First non-empty descriptive field, ordered most→least informative
            // across the decision-kind payloads actually emitted (append prose,
            // intake method/field, sensitivity winner, auto-advance, emit).
            let brief = [
                "fragment",
                "method_prose",
                "winner",
                "value",
                "method",
                "mode",
                "output_dir",
                "stage",
                "rationale",
                "summary",
            ]
            .iter()
            .map(|k| s(g(dec, k)))
            .find(|v| !v.is_empty())
            .unwrap_or("");
            let _ = writeln!(
                m,
                "| {} | {} | {} | {} | {} |",
                cell(s(g(d, "timestamp")), 20),
                cell(kind, 28),
                cell(s(g(d, "actor")), 18),
                cell(s(g(d, "authority")), 24),
                cell(brief, 90),
            );
        }
        let _ = writeln!(m);
    }

    // ── Assumptions ──────────────────────────────────────────────────────
    let assumptions = read_jsonl(&rt.join("assumptions.jsonl"));
    if !assumptions.is_empty() {
        let _ = writeln!(m, "## Assumptions\n");
        let _ = writeln!(
            m,
            "{} assumption(s) the run made explicit, each with a risk level and \
             how it was resolved.\n",
            assumptions.len()
        );
        let _ = writeln!(m, "| ID | Risk | Statement | Resolution |");
        let _ = writeln!(m, "| :---- | :---- | :---- | :---- |");
        for a in &assumptions {
            let res = g(a, "resolution");
            let res_brief = if res.is_string() {
                s(res).to_string()
            } else {
                cell(s(g(res, "kind")), 40)
            };
            let _ = writeln!(
                m,
                "| {} | {} | {} | {} |",
                cell(s(g(a, "id")), 28),
                cell(s(g(a, "risk")), 12),
                cell(s(g(a, "statement")), 110),
                cell(&res_brief, 40),
            );
        }
        let _ = writeln!(m);
    }

    // ── Typed data-flow proofs ───────────────────────────────────────────
    let proofs = read_jsonl(&rt.join("proofs.jsonl"));
    if !proofs.is_empty() {
        let _ = writeln!(m, "## Typed data-flow proofs\n");
        let _ = writeln!(
            m,
            "One port-compatibility proof per edge of the typed task graph — no \
             step consumes an output whose type its producer never declared. \
             **{} edge(s) proven.**\n",
            proofs.len()
        );
        let _ = writeln!(m, "| Producer → Consumer | Port → Port | Kind |");
        let _ = writeln!(m, "| :---- | :---- | :---- |");
        for p in &proofs {
            let _ = writeln!(
                m,
                "| {} → {} | {} → {} | {} |",
                cell(s(g(p, "from_node")), 30),
                cell(s(g(p, "to_node")), 30),
                cell(s(g(p, "from_port")), 24),
                cell(s(g(p, "to_port")), 24),
                cell(s(g(p, "kind")), 20),
            );
        }
        let _ = writeln!(m);
    }

    // ── Validation obligations ───────────────────────────────────────────
    let vrep_raw = read_jsonl(&rt.join("validation-reports.jsonl"));
    // De-duplicate identical rows: an obligation declared on more than one of a
    // task's required artifacts is recorded once per artifact, so the same
    // (task_id, obligation_id, outcome) triple can appear multiple times. Count
    // and list the distinct obligations, preserving first-seen order.
    let mut seen_obl = std::collections::HashSet::new();
    let vrep: Vec<&Value> = vrep_raw
        .iter()
        .filter(|v| {
            seen_obl.insert((
                s(g(v, "task_id")).to_string(),
                s(g(v, "obligation_id")).to_string(),
                s(g(v, "outcome")).to_string(),
            ))
        })
        .collect();
    if !vrep.is_empty() {
        // `outcome` has four serialized forms: `passed`, `failed:…`,
        // `errored:…`, `unimplemented:…`. Anything that is not `passed` is
        // review-worthy — count and list it, so `errored`/`unimplemented`
        // obligations are not hidden behind a clean "0 failed".
        let passed = vrep.iter().filter(|v| s(g(v, "outcome")) == "passed").count();
        let not_passing: Vec<&Value> = vrep
            .iter()
            .copied()
            .filter(|v| s(g(v, "outcome")) != "passed")
            .collect();
        let _ = writeln!(m, "## Validation obligations\n");
        let _ = writeln!(
            m,
            "Grounding & contract obligations checked across tasks: **{} passed, \
             {} not passing** of {} recorded.\n",
            passed,
            not_passing.len(),
            vrep.len()
        );
        if !not_passing.is_empty() {
            let _ = writeln!(m, "| Task | Obligation | Outcome |");
            let _ = writeln!(m, "| :---- | :---- | :---- |");
            for v in not_passing {
                let _ = writeln!(
                    m,
                    "| {} | {} | {} |",
                    cell(s(g(v, "task_id")), 36),
                    cell(s(g(v, "obligation_id")), 36),
                    cell(s(g(v, "outcome")), 120),
                );
            }
            let _ = writeln!(m);
        }
    }

    // ── Re-execution equivalence ─────────────────────────────────────────
    if let Some(rx) = read_json(&rt.join("reexecution.json")) {
        let _ = writeln!(m, "## Re-execution equivalence\n");
        let buckets = g(&rx, "bucket_counts");
        let counts: Vec<String> = buckets
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| format!("{}={}", k, v.as_u64().unwrap_or(0)))
                    .collect()
            })
            .unwrap_or_default();
        if counts.is_empty() {
            let _ = writeln!(
                m,
                "Recorded empty in this package — re-execution is a separate \
                 offline step. Run `ecaa-workflow replay --tier all <package>` \
                 to re-execute the deterministic compute in the recorded \
                 environment snapshot and populate the buckets.\n"
            );
        } else {
            let _ = writeln!(m, "Re-execution artifact buckets: {}.\n", counts.join(", "));
        }
    }

    // ── Repair pass ──────────────────────────────────────────────────────
    if let Some(rs) = read_json(&rt.join("repair-status.json")) {
        let _ = writeln!(m, "## End-of-run repair pass\n");
        let _ = writeln!(
            m,
            "Verdict **{}** after {} round(s).\n",
            cell(s(g(&rs, "verdict")), 24),
            g(&rs, "rounds").as_u64().unwrap_or(0)
        );
        let _ = writeln!(
            m,
            "_This is the end-of-repair snapshot, taken during the original run \
             before the final audit-proof re-record; its counts may differ from \
             the Audit-proof invariants table above. The **Now (current \
             invariant)** column reconciles the two timepoints: it reads each \
             routed item's live status from `audit-proof-report.json` and marks \
             ✅ cleared any invariant a later `replay` / `reexec --reseal` has \
             since brought to `pass` — e.g. the offline re-execution and runcrate \
             steps this pass routed for later._\n"
        );
        if let Some(review) = rs.get("review").and_then(Value::as_array) {
            if !review.is_empty() {
                let _ = writeln!(m, "Items routed to review:\n");
                // The "Where to find & review" column is load-bearing: a reviewer
                // must be able to locate the full record and know how to act. The
                // "Now (current invariant)" column reconciles this historical
                // snapshot against the live (post-reseal) invariant verdicts.
                let _ =
                    writeln!(m, "| Failure | Detail | Now (current invariant) | Where to find & review |");
                let _ = writeln!(m, "| :---- | :---- | :---- | :---- |");
                for r in review {
                    // `failure` is a structured object (task, subject, detail,
                    // source, …), not a string — identify it by task + subject
                    // and surface the concrete `detail` a reviewer needs.
                    let f = g(r, "failure");
                    let task = s(g(f, "task"));
                    let subject = s(g(f, "subject"));
                    let label = match (task.is_empty(), subject.is_empty()) {
                        (false, false) => format!("{task} — {subject}"),
                        (true, false) => subject.to_string(),
                        (false, true) => task.to_string(),
                        (true, true) => s(g(f, "id")).to_string(),
                    };
                    let detail = s(g(f, "detail"));
                    let reason = if detail.is_empty() {
                        s(g(r, "why"))
                    } else {
                        detail
                    };
                    // Reconcile against the live verdict: an audit item whose
                    // invariant is now `pass` was an offline step this snapshot
                    // routed for later and which has since been performed.
                    let now = match review_invariant_id(f).and_then(|id| invariant_status(&ap, id)) {
                        Some("pass") => "✅ cleared — now `pass`".to_string(),
                        Some(st) => format!("now `{st}`"),
                        None => "—".to_string(),
                    };
                    let _ = writeln!(
                        m,
                        "| {} | {} | {} | {} |",
                        cell(&label, 60),
                        cell(reason, 100),
                        cell(&now, 40),
                        cell(&review_locus(f), 130),
                    );
                }
                let _ = writeln!(m);
                let _ = writeln!(
                    m,
                    "_Each item's untruncated record is in the sidecar named in the last \
                     column; open the matching UI tab to review, or re-verify offline with \
                     `ecaa-workflow replay --tier verify <package>`. Every routed item is also \
                     a typed `DecisionRecord` in `runtime/decisions.jsonl`._\n"
                );
            }
        }
    }

    // ── Cost ledger ──────────────────────────────────────────────────────
    let cost_rows = read_jsonl(&rt.join("cost-ledger.jsonl"));
    if let Some(cost) = cost_rows.last() {
        let usd = |k: &str| g(cost, k).as_f64().unwrap_or(0.0);
        let _ = writeln!(m, "## Cost ledger\n");
        let _ = writeln!(
            m,
            "Total run cost **${:.4}** (agent ${:.4} · chat ${:.4} · scorer \
             ${:.4} · side-calls ${:.4}).\n",
            usd("total_cost_usd"),
            usd("agent_cost_usd"),
            usd("chat_cost_usd"),
            usd("scorer_cost_usd"),
            usd("side_call_cost_usd"),
        );
    }

    // ── Catalog coverage ─────────────────────────────────────────────────
    if let Some(cov) = read_json(&rt.join("coverage-statement.json")) {
        let total = g(&cov, "total_branches").as_u64().unwrap_or(0);
        let cat = g(&cov, "catalog_covered").as_u64().unwrap_or(0);
        let prop = g(&cov, "proposal_covered").as_u64().unwrap_or(0);
        let fully = g(&cov, "fully_catalog_covered").as_bool().unwrap_or(false);
        let _ = writeln!(m, "## Catalog coverage\n");
        let _ = writeln!(
            m,
            "Of {} workflow branch(es): **{} catalog-covered**{} · {} \
             proposal-covered. Catalog coverage means every branch maps to a \
             registered method in the apparatus catalog.\n",
            total,
            cat,
            if fully { " (all branches)" } else { "" },
            prop,
        );
    }

    // ── Workflow execution order ─────────────────────────────────────────
    if let Some(eo) = read_json(&rt.join("execution-order.json")) {
        if let Some(order) = eo.get("order").and_then(Value::as_array) {
            let _ = writeln!(m, "## Workflow execution order\n");
            let _ = writeln!(m, "{} task(s), in execution order.\n", order.len());
            let _ = writeln!(m, "| # | Task | Output dir |");
            let _ = writeln!(m, "| :---- | :---- | :---- |");
            for t in order {
                let _ = writeln!(
                    m,
                    "| {} | {} | {} |",
                    g(t, "index").as_u64().unwrap_or(0),
                    cell(s(g(t, "task_id")), 40),
                    cell(s(g(t, "output_dir")), 60),
                );
            }
            let _ = writeln!(m);
        }
    }

    let _ = writeln!(
        m,
        "---\n_Generated by `ecaa-workflow` from the package's `runtime/` \
         sidecars. Re-verify offline with `ecaa-workflow replay --tier verify \
         <package>`._"
    );

    std::fs::write(dst.join("AUDIT-REPORT.md"), m)
        .with_context(|| format!("writing audit report under {}", dst.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The "Items routed to review" table must identify each failure. The
    /// `failure` field of a review item is a structured object (task, subject,
    /// detail, source, …), NOT a string — rendering it through `s()` blanks the
    /// cell. Regression guard: the Failure column must carry the task + subject
    /// and the reviewer-facing detail, not an empty cell.
    #[test]
    fn repair_review_rows_render_failure_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rt = dir.path().join("runtime");
        fs::create_dir_all(&rt).expect("mk runtime");

        let status = serde_json::json!({
            "verdict": "mostly_passing",
            "rounds": 1,
            "review": [
                {
                    "failure": {
                        "id": "abc123",
                        "source": "ClaimMismatch",
                        "class": "narrative_correction",
                        "task": "differential_expression",
                        "subject": "2,197 genes upregulated",
                        "detail": "count claim: narrative says 2197, de_results.tsv has 3993",
                        "retry_count": 1,
                        "status": "InReview"
                    },
                    "why": "unresolved after repair (InReview)"
                },
                {
                    "failure": {
                        "id": "def456",
                        "source": { "InvariantFailure": "claim_completeness" },
                        "class": "coverage_gap",
                        "task": "audit",
                        "subject": "claim_completeness",
                        "detail": "4 claim(s) with empty supported_by and not pending",
                        "retry_count": 1,
                        "status": "InReview"
                    },
                    "why": "unresolved after repair (InReview)"
                }
            ]
        });
        fs::write(
            rt.join("repair-status.json"),
            serde_json::to_string_pretty(&status).expect("ser"),
        )
        .expect("write sidecar");

        write_audit_report(dir.path()).expect("write report");
        let md = fs::read_to_string(dir.path().join("AUDIT-REPORT.md")).expect("read report");

        assert!(
            md.contains("## End-of-run repair pass"),
            "repair section must be present"
        );
        // Failure cell carries the task identity + subject (was blank under the bug).
        assert!(
            md.contains("differential_expression"),
            "review row must render the failing task; got:\n{md}"
        );
        assert!(
            md.contains("2,197 genes upregulated"),
            "review row must render the failure subject; got:\n{md}"
        );
        assert!(
            md.contains("audit") && md.contains("claim_completeness"),
            "invariant-sourced failure must render task + subject; got:\n{md}"
        );
        // The concrete reason (detail) is what a reviewer needs, not boilerplate.
        assert!(
            md.contains("narrative says 2197"),
            "review row must render the failure detail; got:\n{md}"
        );
        // Findability: each item must point at WHERE to find + how to review it.
        assert!(
            md.contains("Where to find & review"),
            "review table must carry the findability column; got:\n{md}"
        );
        assert!(
            md.contains("narrative runtime/outputs/differential_expression/"),
            "claim-mismatch item must point at its narrative dir; got:\n{md}"
        );
        assert!(
            md.contains("verdicts[claim_completeness]"),
            "invariant item must point at its audit-proof-report.json verdict; got:\n{md}"
        );
        // Regression: no review row with a blank leading Failure cell.
        assert!(
            !md.contains("|  | unresolved after repair"),
            "Failure column must not be blank; got:\n{md}"
        );
    }

    /// The repair pass is a HISTORICAL snapshot from the original run; some
    /// items it routes to review (equivalence_failure "Q absent",
    /// substrate_validity "runcrate not run") are offline operator steps that a
    /// later `replay`/`reexec --reseal` performs. The report must reconcile the
    /// two timepoints: each routed item carries a "Now (current invariant)"
    /// cell drawn from the (post-reseal) `audit-proof-report.json` verdicts, and
    /// an item whose invariant is now `pass` is marked cleared — so the section
    /// no longer reads as a contradiction of the invariants table above.
    #[test]
    fn repair_review_annotates_later_cleared_invariants() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rt = dir.path().join("runtime");
        fs::create_dir_all(&rt).expect("mk runtime");

        // Current (post-reseal) verdicts: the two offline invariants now pass;
        // evidence_coverage is still a non-pass warn.
        let ap = serde_json::json!({
            "verdicts": [
                {"id": "equivalence_failure", "status": "pass", "n_violations": 0, "n_inspected": 1},
                {"id": "substrate_validity",  "status": "pass", "n_violations": 0, "n_inspected": 1},
                {"id": "evidence_coverage",   "status": "warn", "n_violations": 62, "n_inspected": 64}
            ]
        });
        fs::write(
            rt.join("audit-proof-report.json"),
            serde_json::to_string_pretty(&ap).expect("ser"),
        )
        .expect("write ap");

        // Original-run repair snapshot routed these BEFORE any re-execution.
        let status = serde_json::json!({
            "verdict": "mostly_passing",
            "rounds": 1,
            "review": [
                {"failure": {"id": "e1", "source": {"InvariantFailure": "equivalence_failure"},
                    "task": "audit", "subject": "equivalence_failure",
                    "detail": "no re-execution performed (Q absent)", "status": "InReview"},
                 "why": "unresolved after repair (InReview)"},
                {"failure": {"id": "s1", "source": {"InvariantFailure": "substrate_validity"},
                    "task": "audit", "subject": "substrate_validity",
                    "detail": "runcrate not run", "status": "InReview"},
                 "why": "unresolved after repair (InReview)"},
                {"failure": {"id": "v1", "source": {"InvariantFailure": "evidence_coverage"},
                    "task": "audit", "subject": "evidence_coverage",
                    "detail": "62 output(s) not referenced and not marked unused", "status": "InReview"},
                 "why": "unresolved after repair (InReview)"},
                {"failure": {"id": "c1", "source": "ClaimMismatch", "class": "narrative_correction",
                    "task": "differential_expression", "subject": "2,197 genes upregulated",
                    "detail": "count claim mismatch", "status": "InReview"},
                 "why": "unresolved after repair (InReview)"}
            ]
        });
        fs::write(
            rt.join("repair-status.json"),
            serde_json::to_string_pretty(&status).expect("ser"),
        )
        .expect("write sidecar");

        write_audit_report(dir.path()).expect("write report");
        let md = fs::read_to_string(dir.path().join("AUDIT-REPORT.md")).expect("read report");

        // New reconciliation column present.
        assert!(
            md.contains("Now (current invariant)"),
            "repair table must carry the current-invariant column; got:\n{md}"
        );
        // The two offline invariants, now pass, are marked cleared on their repair rows
        // (disambiguated from the invariants table by their repair detail text).
        let eq = md
            .lines()
            .find(|l| l.contains("no re-execution performed"))
            .unwrap_or("");
        assert!(
            eq.contains("cleared") && eq.contains("pass"),
            "equivalence_failure routed item must be annotated cleared/pass; got:\n{eq}"
        );
        let sv = md.lines().find(|l| l.contains("runcrate not run")).unwrap_or("");
        assert!(
            sv.contains("cleared"),
            "substrate_validity routed item must be annotated cleared; got:\n{sv}"
        );
        // A still-non-pass invariant is NOT marked cleared.
        let ev = md
            .lines()
            .find(|l| l.contains("62 output(s) not referenced"))
            .unwrap_or("");
        assert!(
            !ev.contains("cleared") && ev.contains("warn"),
            "evidence_coverage (still warn) must NOT be marked cleared; got:\n{ev}"
        );
        // A non-audit failure has no invariant to reconcile against.
        let cm = md
            .lines()
            .find(|l| l.contains("2,197 genes upregulated"))
            .unwrap_or("");
        assert!(
            cm.contains("—"),
            "non-audit item must show no current-invariant status; got:\n{cm}"
        );
    }

    #[test]
    fn review_locus_points_offline_invariants_at_commands() {
        let eq = serde_json::json!({
            "id": "x", "source": {"InvariantFailure": "equivalence_failure"},
            "task": "audit", "subject": "equivalence_failure",
            "detail": "no re-execution performed (Q absent)"
        });
        let sv = serde_json::json!({
            "id": "y", "source": {"InvariantFailure": "substrate_validity"},
            "task": "audit", "subject": "substrate_validity", "detail": "runcrate not run"
        });
        let cm = serde_json::json!({
            "id": "z", "source": "ClaimMismatch", "task": "final_reporting",
            "subject": "KLF15", "detail": "..."
        });
        assert!(
            review_locus(&eq).contains("replay --tier all"),
            "equivalence_failure must name the re-execution command; got: {}",
            review_locus(&eq)
        );
        let svl = review_locus(&sv);
        assert!(
            svl.contains("runcrate") && svl.contains("replay --tier verify"),
            "substrate_validity must name runcrate + the re-verify command; got: {svl}"
        );
        assert!(
            review_locus(&cm).contains("claim-verification.json")
                && review_locus(&cm).contains("final_reporting"),
            "claim mismatch must point at claim-verification.json + its task; got: {}",
            review_locus(&cm)
        );
    }

    /// Validation obligation outcomes have four serialized forms —
    /// `passed`, `failed:…`, `errored:…`, `unimplemented:…`. Anything that is
    /// not `passed` is review-worthy and must be counted as not-passing AND
    /// listed; recognizing only `passed`/`failed*` hides `errored`/
    /// `unimplemented` behind a clean "0 failed".
    #[test]
    fn validation_obligations_count_and_list_all_non_passing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rt = dir.path().join("runtime");
        fs::create_dir_all(&rt).expect("mk runtime");

        let rows = [
            serde_json::json!({"task_id":"t1","obligation_id":"ob_ok","outcome":"passed"}),
            serde_json::json!({"task_id":"t2","obligation_id":"ob_fail","outcome":"failed:threshold not met"}),
            serde_json::json!({"task_id":"t3","obligation_id":"ob_err","outcome":"errored:no annotation table in package"}),
            // Duplicate of the row above (same obligation declared on two
            // artifacts of one task) — must be de-duplicated, not double-counted.
            serde_json::json!({"task_id":"t3","obligation_id":"ob_err","outcome":"errored:no annotation table in package"}),
            serde_json::json!({"task_id":"t4","obligation_id":"ob_todo","outcome":"unimplemented:foo_check"}),
        ];
        let jsonl: String = rows
            .iter()
            .map(|r| serde_json::to_string(r).expect("ser"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(rt.join("validation-reports.jsonl"), jsonl).expect("write sidecar");

        write_audit_report(dir.path()).expect("write report");
        let md = fs::read_to_string(dir.path().join("AUDIT-REPORT.md")).expect("read report");

        // 1 passed, 3 not passing of 4 — must reconcile to the total.
        assert!(
            md.contains("**1 passed, 3 not passing** of 4 recorded"),
            "count line must include errored/unimplemented as not-passing; got:\n{md}"
        );
        // Every non-passing obligation must appear in the table.
        assert!(
            md.contains("errored:no annotation table in package"),
            "errored obligation must be listed; got:\n{md}"
        );
        assert!(
            md.contains("unimplemented:foo_check"),
            "unimplemented obligation must be listed; got:\n{md}"
        );
        assert!(
            md.contains("failed:threshold not met"),
            "failed obligation must still be listed; got:\n{md}"
        );
    }
}
