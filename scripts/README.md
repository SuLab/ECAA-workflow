# Scripts

Operational scripts used by the Make targets and manual end-to-end runs, organized by role. This is the slim OSS surface — there is no GitHub CI; the gates below run locally via `make` and the installed git pre-push hook. Files prefixed with `_` are internal helpers sourced by the user-facing scripts.

## Agent execution

| Script | Purpose |
|---|---|
| `agent-claude.sh` | Invoked by `ecaa-workflow-harness` as the local execution agent. Takes a package directory, delegates the next ready task to Claude Code, and writes the task result back to `WORKFLOW.json`. Defaults to subscription billing via `~/.claude/.credentials.json`. |
| `agent-claude-aws.sh` | AWS executor variant of `agent-claude.sh`, used when the harness delegates compute to AWS instances. |
| `agent-claude-slurm.sh` | SLURM executor variant: submits the task via `sbatch` over SSH; driven from the `SlurmExecutor` backend. |
| `agent-claude-common.sh` | Shared shell library sourced by the three `agent-claude-*.sh` wrappers (credential refresh, container plumbing, result-writeback). |
| `_agent-blas-bootstrap.sh` | Internal: BLAS/LAPACK bootstrap for the agent container. |
| `agent-fixture-plots.sh` | Fixture-executor agent that renders deterministic plots for breadth/QA runs without an LLM. |
| `agent_literature_fetch.py` | Literature-atom retrieval helper (PMC OA / E-utilities) used by the agent at runtime. |
| `run-task-on-instance.sh` | SSM wrapper that runs a single ready task on a provisioned AWS instance; driven from `agent-claude-aws.sh`. |
| `run-task-on-slurm.sh` | SSH + sbatch wrapper that runs a single ready task inside a SLURM job; driven from the SLURM executor. |
| `agent-prompts/` | Prompt templates for the execution agent (`task-execution.md`, `literature-retrieval.md`). |

## Container images

| Script | Purpose |
|---|---|
| `build-bio-min.sh` | Build the `bio-min` agent execution container (`make bio-min` / `make bootstrap`). |
| `build-bio-domain.sh` | Build the larger bio-domain container with heavier toolchains. |
| `build-derived-image.sh` | Build a per-task derived image keyed by atom content hash (flocks the buildkit cache). |
| `build-fixture-packages.sh` | Emit the fixture packages used by breadth/QA runs. |

## Architectural-invariant gates (`make lint`)

| Script | Checks |
|---|---|
| `check-no-tokio-in-core-harness.sh` | No `tokio` dependency leaks into `crates/core` or `crates/harness`. |
| `check-no-hashmap-in-emitter.sh` | The emitter uses `BTreeMap`, never `HashMap` (deterministic output). |
| `check-no-lock-unwrap.sh` | No `lock().unwrap()` on poison-prone mutexes. |
| `check-ts-bindings-fresh.sh` | `ui/src/types/` is in sync with the ts-rs source (run `make types` if it drifts). |

## Determinism / reproducibility

| Script | Purpose |
|---|---|
| `verify-emit-reproducibility.sh` | Emits the same scenario twice and byte-diffs the packages (excluding the conversation/decision logs). |
| `verify-reproducibility.sh` | Broader reproducibility sweep across scenarios. |
| `regenerate-goldens.sh` | Regenerate golden fixtures after an intentional emit-shape change. |
| `refresh-real-fixture.sh` | Refresh a real captured fixture package. |

## ECAA spec / schema validation

| Script | Purpose |
|---|---|
| `spec-check/validate_schemas.sh` | Validate emitted subgraph sidecars against the JSON Schemas in `docs/ecaa-spec/subgraph-schemas/`. |
| `spec-check/owl_consistency.py` | OWL consistency check of the ECAA ontology projection. |
| `spec-check/project_package.py` | Project an emitted package into the ECAA subgraph view for validation. |
| `wrroc-validate.py` | WRROC Tier-3 round-trip validation against the fixture corpus. |

### `runcrate` (substrate-validity bar)

`scripts/wrroc-validate.py` shells out to the WRROC **`runcrate report`** wrapper. Install it with `pip install runcrate` (it provides the `runcrate` console-script entrypoint on `PATH`). With `runcrate` present, `make test-substrate-utility` exercises the `substrate_validity` row of the invariant-utility conformance matrix (and `make conformance` runs it as a non-blocking step). Without `runcrate`, the substrate row is only `Unverified` under the hermetic Noop validator — that row is **only meaningful with `runcrate` installed**, so the substrate invariant is `SKIP`ped and `make conformance` continues without blocking.

## Test drivers

| Script | Make target | Purpose |
|---|---|---|
| `test-e2e.sh` | `make e2e` | Smoke test: build, emit, inspect a small package. |
| `test-chat-confirm.sh` | — | Regression for the deterministic-chat confirmation auto-proceed behavior. |
| `test_agent_claude_common.py` | — | Unit tests for `agent-claude-common.sh` plumbing. |
| `test_build_bio_min.py` | — | Unit tests for the bio-min container build. |
| `test_agent_fixture_plots.py`, `test_harness_fixture_plots.py` | — | Tests for the fixture-plot agent + harness path. |
| `test-atom-proposal.py`, `test-inputs-upload.py`, `test-small-subset-dag.py` | — | Targeted integration checks. |
| `tests/test_agent_literature_fetch.py` | — | Unit tests for `agent_literature_fetch.py`. |

## QA / breadth utilities

| Script | Purpose |
|---|---|
| `audit_dag.py` | Audit an emitted `WORKFLOW.json` for cycles, orphans, modality pollution, and missing terminal reporting. |
| `dag_compare.py` | Diff two emitted DAGs. |
| `claim_verify_injection_harness.py` | Adversarial claim-verification injection harness. |
| `enumerate_plot_atoms.py`, `generate_plot_stubs.py` | Enumerate plot-bearing atoms / scaffold renderer stubs. |
| `migrate_atom_safety.py` | One-shot migration helper for atom `safety:` blocks. |
| `scan_container_network.sh` | Audit container network policy. |
| `scrub-keys-from-traces.sh` | Redact secrets from captured traces. |
| `strip-comment-noise.py` | Strip phase/ticket-prefixed comment noise. |
| `docgen_repo.py` | Repo-root resolver + markdown link audit. |
| `prune-lineage.sh` | Walk `$ECAA_PACKAGE_ROOT` and list (dry-run by default; `APPLY=1` deletes) amendment chains longer than `--keep-last`. |

## Helpers / libraries

| Path | Purpose |
|---|---|
| `helpers/cngb_fetch.py` | CNGB dataset fetch helper. |
| `helpers/provision_r_bioconductor.sh` | Provision R + Bioconductor inside the agent container. |
| `lib/test-helpers.sh` | Shared shell test helpers. |
| `hooks/pre-push` | The repo-local pre-push hook installed by `make install-hooks`; runs `make lint`. |

## Eval harness (`scripts/eval/`, operator-run)

Live evaluation is operator-run only (never invoked by any CI), gated behind `ECAA_EVAL_LIVE=1`, and driven by `make eval TIER={dryrun|e2e|biomnibench|nekrutenko}`. The harness lives under `scripts/eval/` (Python): `eval_runner.py`, `scheduler.py`, `benchmark.py`, `rubric_normalize.py`, plus `plugins/` (BiomniBench + Nekrutenko), `scoring/`, and `services/`. Offline unit tests run via `make eval-tests`. See the `eval-*` targets in `make help`.

## Conventions

Keep scripts fail-fast (`set -euo pipefail`), deterministic by default, and derive the repo root dynamically instead of hardcoding a machine-local path. Do not silently downgrade validation failures to warnings.
