# Scripts

Operational scripts used by the Make targets, CI, and manual end-to-end runs. Organized by role. For the developer-focused script conventions (fail-fast, determinism, session isolation), see [AGENTS.md](AGENTS.md).

## Current (Rust workspace)

These scripts are the current active set, wired into the Rust workspace Make targets.

### Agent harness

| Script | Purpose |
|---|---|
| `agent-claude.sh` | Invoked by `scripps-workflow-harness` as the execution agent. Takes a package directory, delegates the next ready task to Claude Code, and writes the task result back to `WORKFLOW.json`. Requires `ANTHROPIC_API_KEY`. |
| `agent-claude-aws.sh` | AWS executor variant of `agent-claude.sh`. Used when the harness is configured to delegate compute-heavy tasks to AWS instances. See [docs/remote-compute-operator-reference.md](../docs/remote-compute-operator-reference.md). |
| `agent-claude-slurm.sh` | SLURM executor variant of `agent-claude.sh`. Submits the task via `sbatch` over SSH onto the configured partition; driven from the `SlurmExecutor` backend. |
| `agent-mock-blocker.sh` | Deterministic mock blocker agent used by the UI tests and `conversation/tools/execution.rs`. No LLM dependency; emits a canned blocker to exercise `BlockerCard` dispatch paths. |
| `run-task-on-instance.sh` | SSM wrapper that runs a single ready task on a provisioned AWS instance. Driven from `agent-claude-aws.sh`; depends on `aws ssm`. |
| `run-task-on-slurm.sh` | SSH + sbatch wrapper that runs a single ready task inside a SLURM job. Driven from `harness/executor/slurm/sbatch.rs`. |

### Test drivers

| Script | Make target | Purpose |
|---|---|---|
| `test-e2e.sh` | `make e2e` | Smoke test: build, emit, inspect a small package. |
| `test-ivd.sh` | `make ivd`, `make ivd-execute` | IVD real-world compile test. Asserts 25-task DAG and all Fix 1–8 artifacts. `KEEP_PACKAGE=1` preserves output at `/tmp/scripps-ivd-latest`. |
| `test-ivd-chat.sh` | `make ivd-chat` | IVD scenario driven through deterministic `scripps-workflow chat`. Includes the lotz alignment closed-loop assertion via `scripts/helpers/lotz_ledger.py`. |
| `test-ivd-chat-llm.sh` | `make ivd-chat-llm` | IVD scenario driven through LLM-mediated `scripps-workflow chat-llm`. Skipped without `ANTHROPIC_API_KEY`. |
| `test-ivd-web-execute.sh` | `make ivd-web-execute` | Web chat session + harness with `--session-id`, end-to-end with `curl + jq`. Skipped without `ANTHROPIC_API_KEY`. |
| `test-ivd-web-execute-mock.sh` | `make ivd-web-execute-mock` | Mock-backend smoke of the web chat + harness path. No `ANTHROPIC_API_KEY` needed. |
| `test-ivd-cross-version.sh` | `make ivd-cross-version` | IVD v1→v5 cross-version regression. Invoked from `rust.yml` on PR and on demand locally. |
| `test-chat-confirm.sh` | `make chat-confirm` | Regression test for the `awaiting_confirm` auto-proceed behavior. |
| `measure-latency-baseline.sh` | `make latency-baseline` | Fixture latency baseline. Gated on `ECAA_FIXTURE_TIMEOUT_MS` (default 8000 ms). |
| `check-test-counts.sh` | `test-count-check` CI job | Reproduces `.github/ci/expected-test-counts.json` baseline locally; fails on drift-down. |

### Provenance / lifecycle

| Script | Make target | Purpose |
|---|---|---|
| `enrich_provenance.py` | `make enrich PKG=<dir>` | Enriches an emitted package with additional provenance metadata. Requires Python 3.10+. |
| `prune-lineage.sh` | `make prune-lineage [APPLY=1]` | Walks `$ECAA_PACKAGE_ROOT` and lists (dry-run by default) every amendment chain longer than `--keep-last` (default 3); `APPLY=1` deletes. |

### Helpers

| Script | Purpose |
|---|---|
| `helpers/lotz_ledger.py` | Reads the lotz SME ledger at `testdata/IVD_prompt/lotz-sme-decisions.yaml` and exposes two subcommands: `emit-resolves` (emit `/resolve` lines for the deterministic chat REPL) and `drift-report` (compare an emitted `WORKFLOW.json` against the ledger and write `runtime/lotz-alignment-report.md`). Used by `make ivd-chat` and `make ivd-execute`. |

### Repo hygiene

| Script | Purpose |
|---|---|
| `check_repo_hygiene.sh` | Portability, tracked-junk, and compiler-byproduct gate. Run by CI. |
| `docgen_repo.py` | Repo-root resolver, markdown link audit, documentation-governance audit. Backs `check_repo_hygiene.sh` and the `docs-link-audit` / `docs-governance-audit` subcommands. |

## Legacy (pre-Rust migration)

See [legacy/README.md](legacy/README.md) for the three surviving pre-Rust scripts.

## Script conventions

Keep scripts fail-fast (`set -euo pipefail`), deterministic by default, and derive the repo root dynamically instead of hardcoding a machine-local path. Do not silently downgrade validation failures to warnings. For the full guide, see [AGENTS.md](AGENTS.md).
