# Changelog

All notable changes to `ECAA-workflow` land here. Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

> **Note:** This project was renamed from `scripps-workflow` during the open-source split. Historical entries below reference docs and paths that lived in the originating repo and may not be present here.

New entries go at the top. One bullet per user-visible change.

## [Unreleased]

### Deprecated
- **Plan §S9.1 — Phase 4 sunset notice.** `config/stage-taxonomies/`, the `--taxonomy` CLI flag, `expand_includes` / `expand_composition` taxonomy helpers, and the `build_dag_from_taxonomy()` builder entry point are all scheduled for removal in a future release once the archetype-default composer path (`ECAA_COMPOSER=archetypes`) has soaked through ≥2 release cycles with zero regressions and ≥2 novel-composition real-world validations. Concrete deprecation steps will be (1) `--taxonomy` warns when invoked, (2) next cycle: warns with a removal date, (3) third cycle: hard-errors. The `ClassificationResult.taxonomy_path` field will rename to `archetype_id` with a one-cycle `#[serde(alias)]` shim; the additive `archetype_id` field already shipped this cycle (post-S9.4) so live sessions reload across the rename. See `docs/2026-04/unified-implementation-plan-2026-04-28.md` §S9 for the full cutover plan.

### Added
- **In-flow branching from completed tasks (M1.1).** `BranchFromHereCard` now wired into `ConversationPane` for session-scoped branching without leaving the chat flow. `TaskDetailDrawer` "Explore in a branch" entry point (M1.3) enables task-scoped branching; child sessions inherit parent's completed prerequisites and re-run only the named task and its descendants.
- **Per-task agent-generated code capture (M1.2).** `AgentCodeRecord` type introduced to capture agent-generated code at `runtime/outputs/<task_id>/agent-code.json`. Surfaced via new per-task `/result` endpoint and as the "Code" subtab in `TaskDetailDrawer`. Types exported via `make types` (ts-rs).
- **Task-scoped branch lineage (M1.3).** `BranchInput.task_id` + `SessionLineage.branched_from_task_id` enable deterministic child-session derivation from a parent's specific task. Cross-version diff anchors to task pair when set. `prov:wasDerivedFrom` lineage threaded through RO-Crate generation.
- **Docs-as-contract regression tests.** `crates/conversation/tests/documented_constants.rs` asserts that every load-bearing number, greeting string, blocker-title, and Lotz walkthrough fixture reference in `CLAUDE.md` and the SME docs matches the live code. When a doc-referenced constant drifts, CI goes red with a message pointing at the doc line to update.
- **PR template** at `.github/PULL_REQUEST_TEMPLATE.md` with a doc-parity checklist.
- **Operational doc set** — `docs/security-model.md`, `docs/on-call-runbook.md`, `docs/api-reference.md`, `docs/versioning-policy.md`, `docs/observability.md`, `docs/reproducibility.md`.
- **SME-facing doc coverage for post-emit lifecycle features** — Dashboard / History / Cross-Version Diff / Sensitivity / Start Execution cards, quick-reply chips, auto-title, model transparency, rubric scorer, clinical-trial confirmation variant.
- **Cost / time / privacy / non-features section** in `docs/sme-user-guide.md`.
- **`config-reference.md`** now covers `project-class-keywords.yaml`, `_shared/`, `compute-profiles/` (full file list + GPU capability map), and `gene-panels/`.
- **`make verify-reproducibility`** target + `docs/reproducibility.md` — reproduces the IVD intake twice and SHA-compares the emitted packages (excluding the documented audit-log files).

### Changed
- **Root user-facing docs reorganized into three role-separated files.** `README.md` rewritten as a triage entry + operator quickstart. `USERS.md` added at root as a comprehensive SME guide (Part I walkthrough / Part II reference / Part III help). `METHODS.md` added at root as a methods-paper-style whitepaper covering problem statement, related work, system design, the compositional model, reproducibility + provenance, claim verification, the lotz IVD v1→v5 case study, and limitations. `docs/sme-user-guide.md` and `docs/sme-reference.md` removed — their content is in USERS.md. `crates/conversation/tests/documented_constants.rs` path constants updated; the greeting / blocker-title / lotz walkthrough assertions are unchanged in shape.
- **Comment cleanup.** Stripped implementation-timing / process-metadata comments (dated `§N.M` plan pointers, Wave / Phase / Tier labels, PR / SME / Gap / Round-N / Workstream / SME QoL sprint identifiers, `Per plan §X.Y` references, ancestry notes, and dated plan-doc file paths) from ~600 sites across the Rust workspace, `ui/src/`, scripts, Makefile, packer, containers, config YAML/JSON, reference docs, and READMEs. Named architectural invariants (`§R-N`, `§3.X`, `Invariant I-N`) preserved as shared vocabulary. The `comment-hygiene` CI job regex was tightened to catch this class of drift across a wider file-type surface; a policy paragraph was added to `CONTRIBUTING.md`. Two test functions renamed (`pre_wave_sessions_default_to_exploratory` → `legacy_sessions_default_to_exploratory`, `HardwareEnvelopeInputs::local_phase2` → `local_serial`). Audit + remediation plan archived to [`docs/archive/comment-cleanup/`](docs/archive/comment-cleanup/).
- **CLAUDE.md refreshed** for current state: closed-tool vocabulary anchored to `Tool::COUNT` (14 batchable + 8 high-impact), per-modality manifests under `config/modalities/`, 14 state-inspector tabs, 10-iteration tool loop, 60s transcript polling, Haiku 4.5 used for auto-title side-calls.
- **Lotz walkthrough** rewritten to match the transition fixture set — every transition is an `amend_stage_method` with a different motivation (data-shape, annotation-rule, parameter-justification, prior-knowledge-override).
- **Greeting text** now matches what `greeting_turn()` actually returns. Snapshot-tested.
- **`host_error` blocker recovery copy** corrected — no longer mentions a "Resize and resume" button that was stall-specific.

### Fixed
- **DashboardTab empty placeholder** — the dev `<div>empty</div>` that leaked to prod is now a proper styled empty-state message.
- **Adding a modality** in `config-reference.md` now documents the downstream-policy registration step, which was missing from the 4-step list and produced startup errors on follow-through.
- **DAG terminology contradiction** — `sme-user-guide.md` no longer says "see the DAG" (the assistant's voice forbids that term); says "see the task graph" instead.
- **README.md stale tool count** — `chat-llm` subsection now says "16-tool vocabulary" (was "15") and uses `ECAA_ANTHROPIC_API_KEY` (was the legacy `ANTHROPIC_API_KEY`) in every subsection that quoted it; legacy fallback preserved.

### Deprecated
- `ANTHROPIC_API_KEY` (chat-side) — use `ECAA_ANTHROPIC_API_KEY`. The legacy name still works with a one-time stderr warning; removal target not set.

### Deferred
- **Annotated walkthrough screenshot** (`docs/images/sme-walkthrough-overview.png`) — requires running the UI in a browser and capturing a labelled reference frame. Defer until the next UI pass; not blocking since the split-pane layout is self-documenting and every labelled surface is also described in `sme-user-guide.md`.

## [0.1.0] — 2026-04 (pre-release baseline)

The codebase prior to the docs remediation was never formally tagged. Everything up to commit `33369b6` is considered the 0.1.0 baseline for changelog purposes. Functional scope at that point:

- Natural-chat (intake→emit chat, confirmation gate, fixture corpus, latency baseline).
- AWS remote compute, SLURM remote compute, modularity splits.
- Full-lifecycle SME operations: result review, amendment, rerun, sensitivity winner, branching.
- Token-burn remediation: `ECAA_LIVE_API` gate, `ECAA_DISABLE_CONTEXT_EDITING`, `ECAA_SESSION_TOKEN_BUDGET`, `ECAA_SLIM_TAXONOMY`, Opus 4.7 escalation target, 10-iteration tool-loop cap, soft-landing nudge at iteration 7.

Future entries will land here as they ship.
