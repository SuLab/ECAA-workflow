# ECAA-workflow

A deterministic, offline compiler that turns a natural-language description of a bioinformatics analysis into a self-contained, agent-executable [RO-Crate](https://www.researchobject.org/ro-crate/) package — with a full-lifecycle conversational shell wrapped around the executing package.

The compiler classifies the intake, selects an archetype, builds a task DAG, emits a package, and an execution harness drives an agent (Claude Code, a shell script, anything callable with a package path) against the emitted DAG. The emitted package is an **ECAA** (Evidence-Carrying Analysis Artifact) v0.2 package — a typed RO-Crate that carries, alongside the analysis itself, the claims it supports, the evidence backing each claim, and the decision record that produced them. An embedded **ECAA validator** checks the package against a machine-checkable contract over those subgraphs. Emit-time validation defaults to pure-Rust JSON Schema (`schema_only`) and is **advisory (warn-only)** on a plain emit unless `ECAA_VALIDATION_BLOCK_ON_FAIL=1`; the local conformance gate (`make conformance`, `ECAA_CONFORMANCE_MODE=1`) runs it block-on-fail. The contract establishes machine-checkable **consistency** between an analysis's claims, evidence, decisions, and execution provenance — it does **not** establish biological validity; treat verdicts as a hygiene floor, not a quality ceiling.

## Layout

| Component | Crate / dir | Role |
|---|---|---|
| Compiler | `crates/{core, cli}` | Classifier → DAG → emitter. Synchronous, no LLM dependency. |
| Conversation shim | `crates/conversation` | Closed tool vocabulary wraps the compiler. LLM is a UX shim only. |
| Chat server | `crates/server` | Axum HTTP + SSE backend at `/api/chat/*` and `/api/git/*`. |
| Execution harness | `crates/harness` | Loops an agent subprocess against ready tasks. `Local` / `Mock` / `AWS` / `SLURM` executors. |
| ECAA validator | `crates/{ecaa-conformance, ecaa-types}` + `docs/ecaa-spec/` | Emits + validates the ECAA conformance contract. `ecaa-conformance` re-exports core's public API and is the conformance harness a second implementer runs against their own packages — not an independent reimplementation of core. |
| Web UI | `ui/` | React 18 + Vite + TypeScript chat surface. |

Current ECAA profile IRI: `https://w3id.org/ecaa/v0.2`. The active spec files are in [`docs/ecaa-spec/`](docs/ecaa-spec/): `v0.2.md`, `ecaa-v0.2.ttl`, `ecaa-v0.2.shacl.ttl`, and `ecaa-v0.2.jsonld`.

## Setup

Linux x86-64 is the primary supported target. macOS works for dev. Windows requires WSL2.

```bash
# 1. System tools
sudo apt-get install -y build-essential pkg-config libssl-dev mold git curl   # Debian/Ubuntu
# or: sudo dnf install -y @development-tools openssl-devel mold git curl       # Fedora/RHEL
# or: brew install mold openssl@3 pkg-config                                    # macOS

# 2. Rust toolchain (auto-installs the pinned channel from rust-toolchain.toml)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# 3. Cargo dev tools
cargo install --locked cargo-nextest cargo-hakari

# 4. Node.js 20+ (for the UI + Playwright)
# install per your platform; verify: node --version  # ≥ 20

# 5. Build everything
make build           # Rust workspace (debug)
make install         # binaries to ~/.cargo/bin
(cd ui && npm install)
```

`make doctor` prints toolchain readiness; `make help` lists targets.

## Run

Two terminals:

```bash
# terminal A — chat server on :3000
make dev-server

# terminal B — Vite dev UI on :5173 (proxies /api/* to :3000)
make dev-ui
```

> **Execution requires the installed harness binary.** `make dev-server` / `make dev-ui` run from source (`cargo run` / Vite), so they work without `make install`. But the **Start execution** button spawns the `ecaa-workflow-harness` *binary* by name from `PATH` (override with `ECAA_HARNESS_BIN_PATH`). If you skipped `make install` (Setup step 5), execution fails with `failed to spawn harness: No such file or directory`; run `make install` to fix it.

The chat surface boots in offline mode without an API key (the UI renders but assistant turns are mocked). For LLM-mediated chat:

```bash
export ECAA_ANTHROPIC_API_KEY=<your key>
make dev-server
```

For local production configuration, start from [`.env.example`](.env.example). Its active defaults are loopback/local and durable under `$HOME/.ecaa-workflow`; live API, eval, debug, AWS, SLURM, and external-validator knobs are commented until deliberately enabled.

Smoke-test the compiler against a bundled scenario:

```bash
ecaa-workflow intake \
  --input testdata/scenarios/01-bulk-rnaseq-ibd/request.md \
  --output /tmp/ibd-package
ecaa-workflow dag --package /tmp/ibd-package
```

## Operate (container-runtime-only)

With only Docker or (preferred) rootless Podman installed:

```bash
make up                   # build the latest image, then run it via compose + .env on http://127.0.0.1:3000
./deploy/ecaa up          # run the existing local image (no rebuild); same compose + .env
./deploy/ecaa doctor      # preflight: runtime, socket, creds
make down                 # stop it   (make logs to tail)
```

To run analyses, log in to Claude Code once so `~/.claude` holds subscription credentials
(mounted read-only into the server). For shared-server / cloud / HPC, see [`deploy/`](deploy/).

## Test

```bash
make test            # cargo test --workspace
make test-ui         # Vitest + axe a11y
make check           # test + tsc --noEmit
make e2e-playwright  # mocked Playwright tier
```

## Architectural rules

- **Compiler is synchronous.** `tokio` is allowed in `server`, `conversation`, and `cli` (for `serve` only). Never in `core` or `harness`. Harness uses `ureq` (sync).
- **Deterministic output.** Emitted packages are byte-reproducible. Use `BTreeMap`, not `HashMap`. Avoid timestamps and random IDs outside `uuid_short()`.
- **LLM as UX shim.** Closed tool vocabulary (`Tool::COUNT` asserted at compile time). High-impact actions are gated by deterministic server state, not LLM inference.
- **Confirmation discipline.** `emit_package` returns `PreconditionFailure` unless `session.user_confirmed == true`. The button click is a server-side action the LLM observes only via `get_session_state`.
- **ECAA conformance.** Every normal emitted package carries the eight required ECAA v0.2 sidecars: `runtime/intake-conversation.jsonl`, `runtime/decisions.jsonl`, `runtime/validation-reports.jsonl`, `runtime/proofs.jsonl`, `runtime/claim-verification.json`, `runtime/verifier-decisions.jsonl`, `runtime/assumptions.jsonl`, and `runtime/audit-proof-report.json`. There is no reduced ECAA mode switch; `ECAA_ABLATE_*` flags are the only code-backed way to emit non-conformant control artifacts.

## Configuration

`config/` is the source of truth for modalities, archetypes, atoms, compute profiles, gene panels, plot affordances, and downstream-policy contracts. `config/archetypes/` and `config/stage-atoms/` carry their own READMEs. Runtime environment variables are catalogued in [`.env.example`](.env.example).

## Documentation

User guide: [`USERS.md`](USERS.md). Methods summary: [`METHODS.md`](METHODS.md). Contributor guide: [`CONTRIBUTING.md`](CONTRIBUTING.md). ECAA spec: [`docs/ecaa-spec/`](docs/ecaa-spec/).

## License

Apache-2.0 — see [`LICENSE`](LICENSE).
