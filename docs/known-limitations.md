# Known limitations (pre-production, disclosed-not-fixed)

This document tracks defects that an audit surfaced on code paths that are
**not in active use** today. Each item is real, but benign under the current
trust model (single-user `local` execution; no multi-tenant server; no live
AWS/SLURM remote compute). The decision is to **disclose honestly** rather than
fix now. Each entry records WHAT the limitation is, WHERE it lives (verified
file + function), WHY it is acceptable today, and the TRIGGER that promotes it
to in-scope.

Every file path below was grep/ls-verified against the tree at the time of
writing. Line numbers are cited only where the cited symbol was read directly.

---

## Multi-tenant / auth

### srv-01 — harness task-state writes 403/stall under multi-tenant authZ

**What.** The harness authenticates outbound HTTP with a bearer token only; it
never sends the `X-Scripps-User` owner header. The per-session authZ middleware
compares that header against the persisted `Session.owner_user`. On the
single-user `local` default the middleware short-circuits, so the missing
header is harmless. Under a real multi-tenant deployment (sessions owned by a
named user, no `local` sentinel), every harness state-write would be rejected
with `403`, stalling task-state propagation. The intended fix — a harness
self-token principal resolver — exists only as an unwired stub.

**Where.**
- Harness sends only `Authorization: Bearer …`, no owner header:
  `crates/harness/src/progress_client.rs` — the sender job that POSTs to
  `/api/chat/session/{}/task/{}/state` sets only the `Authorization` header
  (the `SetTaskState` arm; bearer set near line 1216). A grep of
  `crates/harness/` for `X-Scripps-User` / `X-Forwarded-User` / `X-Harness-Token`
  returns no matches — the harness emits none of these.
- Middleware comparison + `local` short-circuit:
  `crates/server/src/auth/verify_owner.rs::verify_owner_middleware`
  (fn at line 133). The single-user-dev sentinel
  `LOCAL_OWNER_SENTINEL = "local"` (const at line 57) short-circuits at line
  159 (`if session.owner_user == LOCAL_OWNER_SENTINEL { return next.run(req) }`).
  The strict-compare path (lines 178–201) returns `403`
  (`OwnerAuthzError::HeaderMissing`) when the header is absent for a
  non-`local` session.
- Unwired principal resolver:
  `crates/server/src/auth/principal.rs::resolve_harness_token` (fn at line 292)
  is a stub that ignores its arguments and returns `None` (lines 296–297,
  comment "wired in subsequent task"). Its caller (line 252) treats `None` as
  "not a valid harness token" and falls through to `401` for an
  `X-Harness-Token`-bearing request.

**Why acceptable today.** The default `owner_user` is the `local` sentinel
(no `X-Scripps-User` header at session-create → `local`). The middleware
short-circuits on `local`, so the harness's headerless writes always pass. The
deployment model today is single-user / loopback-bound; there is no second
tenant whose sessions the harness could touch.

**Trigger to fix.** Enabling multi-tenant / shared-server mode — i.e. sessions
created with a real `X-Scripps-User` owner so `owner_user != "local"`. At that
point `resolve_harness_token` must be implemented (issue a harness self-token at
`/start_execution`, store it on the `ExecutionHandle`, accept it here) so the
harness can authenticate as the session's executor without impersonating the
owner.

---

## Remote compute (AWS / SLURM)

### harness-01 — AWS orphan-reaper cannot reap a launched-but-unrecorded instance

**What.** The AWS orphan-reaper cross-checks tag-matched EC2 instances against a
WAL set of instance-ids this harness is known to have launched. Any tag-matched
candidate **absent** from that set is treated as a possible tag-spoof and
skipped (never terminated). But the WAL set is sourced from `WORKFLOW.json`
tasks already in `Running { remote: Some {…} }` state. During the
**launch → first-recorded-iteration** window — after `run-instances` returns an
instance-id but before the task transitions to `Running` with that `remote`
field — the legitimately-launched instance is *not yet* in the WAL set, so the
reaper classifies it as a spoof and skips it. A crash in that window leaks the
instance (cost-leak), because nothing later re-attributes it.

**Where.**
- Cross-check that drops non-WAL candidates as spoofs:
  `crates/harness/src/executor/aws/orphans.rs::wal_cross_check` (fn at line 182).
  When `wal_ids` is non-empty, any candidate not in the set goes to the
  `filtered` (spoof) bucket (lines 191–197). The caller
  `scan_orphans_verified` (fn at line 300) logs each filtered id as a "possible
  tag spoof" and never terminates it (lines 314–319).
- WAL set is built from already-`Running` tasks only:
  `crates/harness/src/dispatch_wal.rs::instance_ids_from_workflow_json`
  (fn at line 260) collects instance-ids only from tasks whose state is
  `TaskState::Running { remote: Some {…} }` (lines 271–272). An instance whose
  task has not yet reached that state contributes no id.
- The launch→record gap:
  `crates/harness/src/executor/aws/provisioning.rs::do_provision` (fn at line
  265) shells out to `run-instances`, parses the `InstanceId`, and stores it on
  `self.instance` (around lines 471–499). It reaches `WORKFLOW.json` only later,
  via the harness's `set_task_state` transition to `Running` — the gap between
  these two steps is the unreapable window.

**Why acceptable today.** The AWS executor is not in active use
(`ECAA_EXECUTOR_MODE=local` is the default). No instances are launched, so the
window never opens. The `empty wal_ids` branch (lines 186–188) also degrades to
tag-only filtering on a fresh package, narrowing the exposure to the specific
mid-run interleaving above.

**Trigger to fix.** AWS executor in active use (`ECAA_EXECUTOR_MODE=aws`).
The fix is to record the instance-id durably *at launch* (e.g. into the
dispatch WAL the moment `run-instances` returns) so the reaper's known-launched
set covers the launch→first-iteration window.

### harness-02 — cumulative cost-guard double-spend under concurrent same-package runs

**What.** The cumulative-spend ceiling is enforced as a non-atomic
read-modify-write split across two calls: a pure-read check, then a separate
read-add-write record. No lock spans the two. The host-level mutual-exclusion
lock the harness holds is keyed by **session_id**, not by package. Two
concurrent harness processes running the *same package* under *different
session-ids* each acquire a distinct lock, so both can pass the read-only check
against the same persisted total and then each record their spend — letting
total spend exceed the configured run-total ceiling.

**Where.**
- Non-atomic check → record:
  `crates/harness/src/executor/cost_guard.rs::CumulativeSpend::check_cumulative`
  (fn at line 340) is a pure read (`current_cumulative()` at line 341, no
  mutation). `record_provision` (fn at line 356) does its own
  read-add-write (`current_cumulative()` at line 363, write+rename at lines
  392–406). The individual file write is atomic (tempfile + rename), but the
  check-then-record *sequence* is not guarded as a unit.
- Lock keyed by session_id, not package:
  `crates/harness/src/multiprocess_lock.rs::SessionLock::acquire` (fn at line 48)
  takes a `session_id` and flocks `~/.ecaa-workflow/locks/<session_id>.lock`
  (module doc lines 1, 12–13). Two sessions over one package get two different
  lockfiles, so neither blocks the other's cost-guard sequence.

**Why acceptable today.** Remote compute is not in active use, so no
provisioning runs through the cost guard at all. Concurrent same-package remote
runs are not part of any current workflow.

**Trigger to fix.** Concurrent remote runs (two harnesses provisioning under
one package/budget simultaneously). The fix is to make the check→record an
atomic critical section keyed by the cost-accounting scope (package / run-total
sidecar), e.g. an advisory file lock around the sidecar that both the check and
the record hold, rather than relying on the per-session lock.

### harness-03 — panic-unsafe remote teardown leaks the instance / job

**What.** Neither the AWS nor the SLURM executor implements `Drop`. Their
`release()` (which terminates the EC2 instance / scancels the SLURM job) is
invoked by an explicit cleanup block on the normal Result-return / error-return
path of the run-loop. A **panic** anywhere in the run-loop call tree unwinds
straight past that block without calling `release()`, leaking the live instance
or job. `panic = "unwind"` is in effect (no `panic = "abort"`), so the unwind
does run destructors — but since there is no `Drop`, there is nothing to run.

**Where.**
- No `Drop` for the remote executors: grep of `crates/harness/src/executor/`
  for `impl Drop` returns only `EnvGuard` (test) and `SystemSshSession`
  (`crates/harness/src/executor/slurm/ssh.rs`, an ssh-session helper) — there is
  no `impl Drop for AwsExecutor` (struct at
  `crates/harness/src/executor/aws/mod.rs:260`) or `impl Drop for SlurmExecutor`
  (struct at `crates/harness/src/executor/slurm/mod.rs:141`).
- `release()` runs only on the explicit cleanup block:
  `crates/harness/src/main.rs` — the "Always run cleanup, even on error /
  early-return" block calls `guard.release()` (lines 1658–1668). This is plain
  sequential code at the end of the loop body, not a guard that survives an
  unwind. The SIGINT handler's `guard.release()` (around line 3923) covers
  signals, not panics.
  `Executor::release` is declared at
  `crates/harness/src/executor/mod.rs:438`; the AWS/SLURM impls delegate to
  `do_release` (`aws/mod.rs:657`, `slurm/mod.rs:1083`).
- `panic = "unwind"` is current: `Cargo.toml` documents `panic = "abort"` as
  DEFERRED and `panic = "unwind"` as the present default (lines 283–285); no
  profile sets `panic = "abort"`.

**Why acceptable today.** Remote executors are not in active use, so no live
EC2 instance or SLURM job exists to leak on a panic. Locally there is nothing to
release beyond a subprocess the OS reaps on exit.

**Trigger to fix.** Remote executor in active use (`ECAA_EXECUTOR_MODE=aws` or
`slurm`). The fix is an `impl Drop` on the AWS/SLURM executors that calls
`release()` / `do_release()` (idempotent with the existing explicit cleanup) so
a panic in the run-loop still terminates the instance / job during unwind.

---

## Network isolation

### harness-05 — atom network policy is a reachability minimum, not an egress ceiling (intentional)

**What.** The per-atom `safety.network` policy is enforced as a *minimum
reachability requirement*, not a *maximum egress ceiling*. The compatibility
check treats an egress-restricted atom (`None { allowlist }`) as satisfiable by
a `Bridge` (full-egress) executor — the atom asks for "at most this set", the
executor offers "at least Bridge", and Bridge ⊇ any allowlist, so the check
passes. The `LocalExecutor` always advertises `NetworkPolicy::Bridge`.
Consequently, under the default local executor an egress-restricted atom still
runs with full host egress. This is **intentional** — but worth knowing,
because `safety.network=None` does NOT, by itself, deny egress.

**Where.**
- Compatibility treats Bridge as satisfying a restricted atom:
  `crates/harness/src/executor/mod.rs::network_compatible` (fn at line 149).
  Arm `(NetworkPolicy::None { .. }, NetworkPolicy::Bridge) => true` (line 153);
  the doc-comment states the semantics explicitly: "MIN-set requirement … not a
  maximum-set ceiling on egress" (around line 144). Used by
  `enforce_safety_policy` (fn at line 86), which only emits
  `BlockerKind::NetworkPolicyMismatch` (line 125) when the executor offers
  *less* than the atom requires (e.g. `Bridge` atom on a `None` executor),
  never when the executor offers *more*.
- LocalExecutor always advertises Bridge:
  `crates/harness/src/executor/local.rs` — `capabilities()` (fn at line 707)
  returns `network: NetworkPolicy::Bridge` (line 733); the doc notes "the host
  has the same egress as the operator's shell" (line 706).
- The blunt mitigation (opt-in, package-wide):
  `crates/harness/src/sandbox_enforcer.rs::render_args` (fn at line 314) pushes
  `--unshare-net` only when `policy.deny_network` is set (lines 375–377) — a
  bwrap network-namespace cutoff that denies all egress for the sandboxed run,
  enabled via `ECAA_LOCAL_SANDBOX=bubblewrap`. The harness prints a startup
  warning making the MIN-not-MAX semantics observable:
  `crates/harness/src/main.rs` lines 1257–1263 ("atom-level
  `safety.network=None` declarations are MIN-not-MAX — they do NOT block host
  network egress. Set ECAA_LOCAL_SANDBOX=bubblewrap to enforce egress deny via
  `--unshare-net` on declared atoms.").

**Why acceptable today.** This is by design: per-atom egress allowlists are
advisory reachability hints, and the deny-egress enforcement is a deliberate,
opt-in, package-wide control (`ECAA_LOCAL_SANDBOX=bubblewrap` → `--unshare-net`)
intended for clinical bundles where egress must be cut off entirely. The startup
warning makes the implicit semantics observable to operators who expected
per-atom denial.

**Trigger to fix / re-evaluate.** If per-atom egress *denial* (a true egress
ceiling honored per task on the default local executor, without the blunt
package-wide `--unshare-net`) ever becomes a requirement — e.g. running mixed
trusted/untrusted atoms in one bundle where only some must be air-gapped — the
enforcement model would need per-task network-namespace isolation rather than
the current all-or-nothing bwrap switch.
