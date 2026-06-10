# Changelog

All notable changes to `ECAA-workflow` land here. Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

> **Note:** This project was renamed from `scripps-workflow` during the open-source split, and this repo is a deliberately **slim OSS surface**. Many historical entries below were authored in the originating (fuller) repo and reference docs, CI, make targets, or paths that are **intentionally absent here**. There is **no GitHub CI** in this OSS repo: gating is local via `make` targets (`make lint` + `make test` + `make conformance`) and the installed git pre-push hook. Bullets describing originating-repo artifacts are marked _(originating repo; paths may be absent here)_ — treat them as a record of the change, not a claim that the artifact exists in this checkout.

New entries go at the top. One bullet per user-visible change.

## [Unreleased]

### Removed
- **Taxonomy path retired.** `config/stage-taxonomies/` is gone, the `--taxonomy` CLI flag is no longer parsed (only `--archetype` is accepted), and the legacy taxonomy-builder entry point was removed. The archetype-driven v4 proof-carrying planner is the sole path; `ClassificationResult` now carries `archetype_id`.

### Added
- **In-flow branching from completed tasks (M1.1).** `BranchFromHereCard` now wired into `ConversationPane` for session-scoped branching without leaving the chat flow. `TaskDetailDrawer` "Explore in a branch" entry point (M1.3) enables task-scoped branching; child sessions inherit parent's completed prerequisites and re-run only the named task and its descendants.
- **Per-task agent-generated code capture (M1.2).** `AgentCodeRecord` type introduced to capture agent-generated code at `runtime/outputs/<task_id>/agent-code.json`. Surfaced via new per-task `/result` endpoint and as the "Code" subtab in `TaskDetailDrawer`. Types exported via `make types` (ts-rs).
- **Task-scoped branch lineage (M1.3).** `BranchInput.task_id` + `SessionLineage.branched_from_task_id` enable deterministic child-session derivation from a parent's specific task. Cross-version diff anchors to task pair when set. `prov:wasDerivedFrom` lineage threaded through RO-Crate generation.
- **Docs-as-contract regression tests.** `crates/conversation/tests/session/documented_constants.rs` asserts that load-bearing numbers, greeting strings, and blocker titles in the SME docs match the live code. When a doc-referenced constant drifts, `make test` fails locally with a message pointing at the doc line to update.
- **PR template + operational doc set** _(originating repo; paths may be absent here)_ — a `.github/PULL_REQUEST_TEMPLATE.md` doc-parity checklist and `docs/{security-model,on-call-runbook,api-reference,versioning-policy,observability,reproducibility}.md`. None ship in this OSS surface; the only tracked `docs/` subtree is `docs/ecaa-spec/`.
- **SME-facing doc coverage for post-emit lifecycle features** — Dashboard / History / Cross-Version Diff / Sensitivity / Start Execution cards, quick-reply chips, auto-title, model transparency, rubric scorer, clinical-trial confirmation variant. In this repo the SME guide is `USERS.md`.
- **Reproducibility check** — `scripts/verify-reproducibility.sh` reproduces an intake twice (now across multiple scenarios + one amend) and SHA-compares the emitted packages (excluding the documented audit-log files).

### Changed
- **Root user-facing docs reorganized.** `README.md` rewritten as a triage entry + operator quickstart. `USERS.md` added at root as a comprehensive SME guide (Part I walkthrough / Part II reference / Part III help). The originating repo also added a root `METHODS.md` whitepaper _(originating repo; absent in this OSS surface)_. The `documented_constants` test (now at `crates/conversation/tests/session/`) path constants updated; the greeting / blocker-title assertions are unchanged in shape.
- **Comment cleanup.** Stripped implementation-timing / process-metadata comments (dated `§N.M` plan pointers, Wave / Phase / Tier labels, PR / SME / Gap / Round-N / Workstream / SME QoL sprint identifiers, `Per plan §X.Y` references, ancestry notes, and dated plan-doc file paths) from ~600 sites across the Rust workspace, `ui/src/`, scripts, Makefile, packer, containers, config YAML/JSON, reference docs, and READMEs. Named architectural invariants (`§R-N`, `§3.X`, `Invariant I-N`) preserved as shared vocabulary. A comment-hygiene policy paragraph was added to `CONTRIBUTING.md`. Two test functions renamed (`pre_wave_sessions_default_to_exploratory` → `legacy_sessions_default_to_exploratory`, `HardwareEnvelopeInputs::local_phase2` → `local_serial`). _(The originating-repo comment-hygiene gate script + archive are not part of this OSS surface.)_
- **CLAUDE.md refreshed** for current state: closed-tool vocabulary anchored to `Tool::COUNT` (14 batchable + 8 high-impact), per-modality manifests under `config/modalities/`, 14 state-inspector tabs, 10-iteration tool loop, 60s transcript polling, Haiku 4.5 used for auto-title side-calls.
- **Lotz walkthrough** rewritten to match the transition fixture set — every transition is an `amend_stage_method` with a different motivation (data-shape, annotation-rule, parameter-justification, prior-knowledge-override).
- **Greeting text** now matches what `greeting_turn()` actually returns. Snapshot-tested.
- **`host_error` blocker recovery copy** corrected — no longer mentions a "Resize and resume" button that was stall-specific.

### Fixed
- **DashboardTab empty placeholder** — the dev `<div>empty</div>` that leaked to prod is now a proper styled empty-state message.
- **Adding a modality** documentation now records the downstream-policy registration step that was missing from the original list _(originating-repo `config-reference.md`; the configuration reference here is the `config/` tree itself)_.
- **DAG terminology contradiction** — SME docs no longer say "see the DAG" (the assistant's voice forbids that term); they say "see the task graph" instead.
- **README.md tool-count + key-name cleanup** — README quotes `ECAA_ANTHROPIC_API_KEY` (not the legacy `ANTHROPIC_API_KEY`; legacy fallback preserved) and no longer hardcodes a stale tool count. The closed vocabulary is `Tool::COUNT` = 22 (14 batchable + 8 high-impact), asserted at compile time; earlier "15/16-tool" mentions were stale.

### Deprecated
- `ANTHROPIC_API_KEY` (chat-side) — use `ECAA_ANTHROPIC_API_KEY`. The legacy name still works with a one-time stderr warning; removal target not set.

### Deferred
- **Annotated walkthrough screenshot** (`docs/images/sme-walkthrough-overview.png`) — requires running the UI in a browser and capturing a labelled reference frame. Defer until the next UI pass; not blocking since the split-pane layout is self-documenting and every labelled surface is also described in `sme-user-guide.md`.

### Removed
- **Composer v1/v2/v3 retired.** v4 (proof-carrying planner) is now the sole planner: the legacy `backward_chain_compose` engine and legacy dispatch paths were removed. `ECAA_COMPOSER` and `Session::composer_version` are retained as compatibility shims — every value (incl. legacy `legacy`/`archetypes`/`backward-chain`) resolves to v4, with a `tracing::warn!` on legacy values. Old session files load unchanged; already-emitted packages are unaffected.

## [0.1.0] — 2026-04 (pre-release baseline)

The codebase prior to the docs remediation was never formally tagged. Everything up to commit `33369b6` is considered the 0.1.0 baseline for changelog purposes. Functional scope at that point:

- Natural-chat (intake→emit chat, confirmation gate, fixture corpus, latency baseline).
- AWS remote compute, SLURM remote compute, modularity splits.
- Full-lifecycle SME operations: result review, amendment, rerun, sensitivity winner, branching.
- Token-burn remediation: `ECAA_LIVE_API` gate, `ECAA_DISABLE_CONTEXT_EDITING`, `ECAA_SESSION_TOKEN_BUDGET`, `ECAA_SLIM_TAXONOMY`, Opus 4.7 escalation target, 10-iteration tool-loop cap, soft-landing nudge at iteration 7.

Future entries will land here as they ship.
