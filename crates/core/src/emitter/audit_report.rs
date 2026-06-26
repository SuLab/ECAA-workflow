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
    let vrep = read_jsonl(&rt.join("validation-reports.jsonl"));
    if !vrep.is_empty() {
        let passed = vrep.iter().filter(|v| s(g(v, "outcome")) == "passed").count();
        let failed: Vec<&Value> = vrep
            .iter()
            .filter(|v| s(g(v, "outcome")).starts_with("failed"))
            .collect();
        let _ = writeln!(m, "## Validation obligations\n");
        let _ = writeln!(
            m,
            "Grounding & contract obligations checked across tasks: **{} passed, \
             {} failed** of {} recorded.\n",
            passed,
            failed.len(),
            vrep.len()
        );
        if !failed.is_empty() {
            let _ = writeln!(m, "| Task | Obligation | Outcome |");
            let _ = writeln!(m, "| :---- | :---- | :---- |");
            for v in failed {
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
        if let Some(review) = rs.get("review").and_then(Value::as_array) {
            if !review.is_empty() {
                let _ = writeln!(m, "Items routed to review:\n");
                let _ = writeln!(m, "| Failure | Why |");
                let _ = writeln!(m, "| :---- | :---- |");
                for r in review {
                    let _ = writeln!(
                        m,
                        "| {} | {} |",
                        cell(s(g(r, "failure")), 60),
                        cell(s(g(r, "why")), 110),
                    );
                }
                let _ = writeln!(m);
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
