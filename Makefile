# ecaa-workflow Makefile — slim build/test surface for the open-source product.
#
# Conventions:
#   - All `cargo` calls go through default-members (the 4 binaries + ECAA validator).
#   - All `npm` calls run from ui/ (set by `cd ui &&` prefix).
#   - `make help` lists the canonical targets with one-line descriptions.

.PHONY: help build build-release install bootstrap test test-runner test-doc \
        test-fast test-core test-conversation test-harness test-server test-cli \
        test-ui conformance test-substrate-utility roc-gate lint-ui clippy fmt check types e2e e2e-playwright deposit-check bench \
        verify-reproducibility \
        bio-min dev-server dev-ui clean doctor lint deny install-hooks \
        image up down logs release-image release-sbom release-sign release-checksums release-publish release \
        eval eval-dryrun eval-e2e eval-full \
        eval-biomnibench eval-biomnibench-smoke eval-nekrutenko eval-nekrutenko-smoke eval-tests \
        eval-biomnibench-dryrun eval-nekrutenko-dryrun \
        eval-publish schema-burden eval-campaign

help: ## Print this help.
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z0-9_-]+:.*?## / {printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

# ── Build ────────────────────────────────────────────────────────────────────

build: ## cargo build (default-members; debug profile)
	cargo build

build-release: ## cargo build --release (default-members)
	cargo build --release

install: ## Install binaries to ~/.cargo/bin
	cargo install --path crates/cli --locked
	cargo install --path crates/server --locked
	cargo install --path crates/harness --locked

bootstrap: build install bio-min ## Build + install binaries + build the bio-min container

bio-min: ## Build the agent execution container (bio-min)
	bash scripts/build-bio-min.sh bio-min:local

ROCM_AGENT_TAG ?= ghcr.io/scripps/bio-min-rocm:v0.1.0
rocm-agent: ## Build the ROCm-enabled agent image for AMD GPUs (multi-GB; operator-run). Override ROCM_AGENT_TAG / ROCM_IMAGE.
	docker build -t $(ROCM_AGENT_TAG) $(if $(ROCM_IMAGE),--build-arg ROCM_IMAGE=$(ROCM_IMAGE),) -f docker/rocm-agent.Dockerfile docker/

# ── Release ──────────────────────────────────────────────────────────────────

SERVER_IMAGE ?= ghcr.io/scripps/ecaa-workflow-server:local
BIO_MIN_IMAGE ?= ghcr.io/scripps/bio-min:local
image: ## Build the server OCI image locally (single-arch)
	bash scripts/build-server-image.sh ecaa-workflow-server:local

up: image ## Build the latest server image, then run it via compose + .env (deploy/ecaa up)
	bash deploy/ecaa up

down: ## Stop the running server container (deploy/ecaa down)
	bash deploy/ecaa down

logs: ## Follow the running server container logs (deploy/ecaa logs)
	bash deploy/ecaa logs

release-image: ## Build + push multi-arch server + bio-min images to GHCR (operator-run)
	bash scripts/build-server-image.sh $(SERVER_IMAGE) --push
	bash scripts/build-bio-min.sh $(BIO_MIN_IMAGE) --push

release-sbom: ## SBOMs + vuln scan for the images (operator-run; needs syft + grype)
	mkdir -p dist/sbom
	syft "$(SERVER_IMAGE)" -o spdx-json=dist/sbom/server.spdx.json -o cyclonedx-json=dist/sbom/server.cdx.json
	-grype sbom:dist/sbom/server.spdx.json

release-sign: ## cosign-sign images + attach SBOM (operator-run; needs cosign + COSIGN_KEY)
	cosign sign --yes --key "$(COSIGN_KEY)" "$(SERVER_IMAGE)"
	cosign attest --yes --key "$(COSIGN_KEY)" --type spdxjson --predicate dist/sbom/server.spdx.json "$(SERVER_IMAGE)"

release-checksums: ## SHA256SUMS over release assets in dist/
	cd dist && sha256sum sbom/* > SHA256SUMS && echo "wrote dist/SHA256SUMS"

release-publish: ## Create a GitHub release with local assets (operator-run; needs gh + TAG)
	gh release create "$(TAG)" dist/SHA256SUMS dist/sbom/*.json --title "$(TAG)" --notes "ECAA-workflow $(TAG). Verify: sha256sum -c SHA256SUMS."

release: ## Full local release (operator-run): TAG=vX.Y.Z make release
	@git describe --dirty --always | grep -q -- '-dirty' && { echo "refusing: dirty tree"; exit 1; } || true
	@test -n "$(TAG)" || { echo "set TAG=vX.Y.Z"; exit 1; }
	$(MAKE) release-image
	$(MAKE) release-sbom
	-$(MAKE) release-sign
	$(MAKE) release-checksums
	$(MAKE) release-publish TAG=$(TAG)

# ── Test ─────────────────────────────────────────────────────────────────────

test: test-runner test-doc ## Run cargo test + doc tests across the workspace

test-runner: ## cargo nextest run --workspace
	cargo nextest run --workspace

test-doc: ## Doc tests
	cargo test --workspace --doc

test-fast: ## Run only unit-fast tests (skip integration where possible)
	cargo test --workspace --lib

test-core: ## Unit + integration for crates/core
	cargo test -p ecaa-workflow-core

test-conversation: ## Unit + integration for crates/conversation
	cargo test -p ecaa-workflow-conversation

test-harness: ## Unit + integration for crates/harness
	cargo test -p ecaa-workflow-harness

test-server: ## Unit + integration for crates/server
	cargo test -p ecaa-workflow-server

test-cli: ## Unit + integration for crates/cli
	cargo test -p ecaa-workflow-cli

test-ui: ## Vitest + axe a11y for ui/
	cd ui && npm run test

conformance: ## ECAA conformance suite (block-on-fail; ECAA_CONFORMANCE_MODE=1 + ECAA_VALIDATION_BLOCK_ON_FAIL=1)
	ECAA_CONFORMANCE_MODE=1 ECAA_VALIDATION_BLOCK_ON_FAIL=1 cargo test -p ecaa-workflow-conformance
	@command -v runcrate >/dev/null 2>&1 && $(MAKE) test-substrate-utility || echo "[conformance] runcrate absent — substrate-utility row skipped (non-blocking)"
	@test -x .venv-validator/bin/python && PATH="$$(pwd)/target/debug:$$PATH" $(MAKE) roc-gate || echo "[conformance] .venv-validator absent — roc-gate skipped (run: python3 -m venv .venv-validator && .venv-validator/bin/pip install -r requirements-validator.txt)"

test-substrate-utility: ## Run the runcrate-gated substrate row of the invariant-utility matrix
	@command -v runcrate >/dev/null 2>&1 || { echo "runcrate not on PATH — install the WRROC runcrate report wrapper first (see scripts/README.md)"; exit 1; }
	ECAA_CONFORMANCE_MODE=1 cargo test -p ecaa-workflow-conformance invariant_utility -- --nocapture

roc-gate: ## Execution-aware strict roc-validator regression gate (T8): plan ro-crate-1.1 + executed all-four profiles + gate-bites proof
	@test -x .venv-validator/bin/python || { echo "[roc-gate] ERROR: .venv-validator missing — run: python3 -m venv .venv-validator && .venv-validator/bin/pip install -r requirements-validator.txt"; exit 1; }
	@command -v ecaa-workflow >/dev/null 2>&1 || { echo "[roc-gate] ERROR: ecaa-workflow not on PATH — run: cargo build --bin ecaa-workflow && PATH=\"\$$(pwd)/target/debug:\$$PATH\" make roc-gate"; exit 1; }
	bash scripts/roc-gate.sh

# ── Lint / format / type-check ───────────────────────────────────────────────

fmt: ## cargo fmt --all
	cargo fmt --all

clippy: ## cargo clippy --workspace
	cargo clippy --workspace -- -D warnings

lint-ui: ## eslint over ui/src
	cd ui && npm run lint

lint: ## Run architectural-invariant + ts-binding + supply-chain gates
	bash scripts/check-no-tokio-in-core-harness.sh
	bash scripts/check-no-hashmap-in-emitter.sh
	bash scripts/check-ts-bindings-fresh.sh
	bash scripts/check-no-lock-unwrap.sh
	bash scripts/check-atom-contracts.sh
	cargo deny check
	@command -v cargo-hakari >/dev/null 2>&1 && cargo hakari verify || echo "[lint] cargo-hakari absent — workspace-hack sync check skipped (cargo install cargo-hakari to enable)"

deny: ## cargo-deny supply-chain gate (advisories/bans/licenses/sources)
	cargo deny check

install-hooks: ## Install the repo-local git pre-push hook
	install -m 0755 scripts/hooks/pre-push "$$(git rev-parse --git-path hooks)/pre-push"
	@echo "pre-push hook installed -> $$(git rev-parse --git-path hooks)/pre-push"

check: test ## test + TypeScript noEmit
	cd ui && npx tsc --noEmit

types: ## Regenerate ts-rs TypeScript bindings into ui/src/types/
	cargo test -p ecaa-workflow-core export_bindings
	cargo test -p ecaa-workflow-conversation export_bindings

# ── End-to-end ───────────────────────────────────────────────────────────────

e2e: ## Quick smoke: build + emit + inspect a sample package
	bash scripts/test-e2e.sh

deposit-check: ## Gate a deposit dir against its DEPOSIT-READINESS.json (DIR=<path> [STRICT=1])
	cargo run --quiet -p ecaa-workflow-cli --bin ecaa-workflow -- deposit-check $(DIR) $(if $(STRICT),--strict,)

e2e-playwright: ## Playwright mocked tier
	cd e2e && npm install && npx playwright install --with-deps && npx playwright test

# ── Dev servers ──────────────────────────────────────────────────────────────

dev-server: ## Run ecaa-workflow-server on :3000
	cargo run -p ecaa-workflow-server -- --port 3000

dev-ui: ## Run the Vite dev server on :5173 (proxies /api/* to :3000)
	cd ui && npx vite

# ── Misc ─────────────────────────────────────────────────────────────────────

bench: ## Criterion benches under crates/core
	cargo bench -p ecaa-workflow-core

verify-reproducibility: ## Assert emitted packages are byte-reproducible across repeated emits
	bash scripts/verify-reproducibility.sh

# ── Eval suite (operator-run; never CI) ───────────────────────────────────────
# Every eval target is gated on ECAA_EVAL_LIVE=1 (the runner prints SKIP and exits
# 0 otherwise) and, for the LLM-judged BiomniBench arm, GEMINI_API_KEY +
# ECAA_ANTHROPIC_API_KEY. Pass extra runner flags via EVAL_ARGS, e.g.
#   make eval-biomnibench EVAL_ARGS="--trials 5 --max-iterations 30"
# The `eval` dispatcher runs one tier; TIER selects which (default: dryrun).

PYTHON ?= python3
TIER ?= dryrun
# Parallelism for `eval-full`. Safe-on-this-host defaults tuned together with the
# per-benchmark memory caps below; invariant N×mem_cap_GB ≤ ~115 and N×nproc ≤ 48
# (48-core / ~125 GB host). Raise only if RAM + API rate limits allow.
NEK_PARALLEL ?= 8
BBENCH_PARALLEL ?= 4

eval: ## Run an eval tier: make eval TIER={dryrun|e2e|biomnibench|nekrutenko} (default dryrun)
	@$(MAKE) --no-print-directory eval-$(TIER)

eval-dryrun: ## Tier 1 — dry run: 1-task smoke of BOTH benchmarks x both arms (cheap plumbing check)
	@$(PYTHON) -m scripts.eval.eval_runner biomnibench --smoke --arms ecaa,claude-direct $(EVAL_ARGS)
	@$(PYTHON) -m scripts.eval.eval_runner nekrutenko --smoke --arms ecaa,claude-direct $(EVAL_ARGS)

eval-e2e: ## Tier 2 — full e2e: BiomniBench (full trials) + Nekrutenko (full + 36-cell fault matrix); costly
	@$(PYTHON) -m scripts.eval.eval_runner biomnibench $(EVAL_ARGS)
	@$(PYTHON) -m scripts.eval.eval_runner nekrutenko --error-matrix $(EVAL_ARGS)

eval-full: ## Both benchmarks IN FULL, fast-and-safe + PARALLEL (operator-run; needs ECAA_EVAL_LIVE=1 + GEMINI_API_KEY + ECAA_ANTHROPIC_API_KEY). Resumable: EVAL_ARGS="--resume <run_dir>".
	@# Run the two benchmarks SEQUENTIALLY (not concurrently) so their CPU/RAM
	@# budgets don't stack. Each sets a per-agent memory cap matched to its
	@# parallelism: Nekrutenko cells are light (~chrM), BiomniBench loads 16-40 GB
	@# matrices. Disk/cache (ECAA_EVAL_CACHE_DIR/RUNS_DIR) come from your .env —
	@# point them at the large mount holding the dataset snapshot. NOTE: if your
	@# .env force-sets ECAA_AGENT_MEMORY_CAP_GB higher, it may override these caps.
	@echo "[eval-full] Nekrutenko — full + 36-cell error matrix, --max-parallel $(NEK_PARALLEL)"
	@ECAA_HW_DYNAMIC_ALLOCATION=0 ECAA_AGENT_MEMORY_CAP_GB=8 ECAA_HW_NPROC_HINT=4 \
		$(PYTHON) -m scripts.eval.eval_runner nekrutenko --error-matrix --trials 3 \
		--max-parallel $(NEK_PARALLEL) $(EVAL_ARGS)
	@echo "[eval-full] BiomniBench — full 50 public tasks, --max-parallel $(BBENCH_PARALLEL)"
	@ECAA_HW_DYNAMIC_ALLOCATION=0 ECAA_AGENT_MEMORY_CAP_GB=28 ECAA_HW_NPROC_HINT=10 \
		$(PYTHON) -m scripts.eval.eval_runner biomnibench --trials 3 \
		--max-parallel $(BBENCH_PARALLEL) $(EVAL_ARGS)

eval-baseline: ## Tractable baseline (live; operator-run): both benchmarks, both arms, 1 trial, SEQUENTIAL, each agent fills the host (ECAA_HW_FILL_HEADROOM=1). Subset pinned in scripts/eval/subsets/baseline.toml. Needs ECAA_EVAL_LIVE=1 + GEMINI_API_KEY + ECAA_ANTHROPIC_API_KEY.
	@MANIFEST=scripts/eval/subsets/baseline.toml ; \
	 TASKS=$$($(PYTHON) -c "import tomllib,pathlib;print(','.join(tomllib.loads(pathlib.Path('$$MANIFEST').read_text())['biomnibench']['task_ids']))") ; \
	 echo "[eval-baseline] BiomniBench subset: $$TASKS  (sequential, ECAA_HW_FILL_HEADROOM=1)" ; \
	 ECAA_HW_FILL_HEADROOM=1 $(PYTHON) -m scripts.eval.eval_runner biomnibench --tasks "$$TASKS" --trials 1 --arms ecaa,claude-direct $(EVAL_ARGS)
	@echo "[eval-baseline] Nekrutenko base + 12-cell matrix (1 seed), sequential, ECAA_HW_FILL_HEADROOM=1"
	@ECAA_HW_FILL_HEADROOM=1 ECAA_EVAL_NEK_SEEDS=42 $(PYTHON) -m scripts.eval.eval_runner nekrutenko --error-matrix --trials 1 --arms ecaa,claude-direct $(EVAL_ARGS)

BENCH ?= biomnibench
TRIALS ?= 3
MODEL ?= claude-sonnet-4-6
TASKS ?=

eval-tractable: ## Tractable eval on a chosen model (default Sonnet): make eval-tractable BENCH=biomnibench TRIALS=3 MODEL=claude-sonnet-4-6 TASKS="da-8-1,da-15-1" (empty TASKS = baseline.toml subset). Needs ECAA_EVAL_LIVE=1 (+ judge keys for biomnibench).
	@TASKS="$(TASKS)" ; \
	 if [ -z "$$TASKS" ]; then \
	   MANIFEST=scripts/eval/subsets/baseline.toml ; \
	   TASKS=$$($(PYTHON) -c "import tomllib,pathlib;print(','.join(tomllib.loads(pathlib.Path('$$MANIFEST').read_text()).get('$(BENCH)',{}).get('task_ids',[])))") ; \
	 fi ; \
	 echo "[eval-tractable] $(BENCH) model=$(MODEL) trials=$(TRIALS) tasks: $${TASKS:-<all (benchmark default)>}" ; \
	 ECAA_HW_FILL_HEADROOM=1 $(PYTHON) -m scripts.eval.eval_runner $(BENCH) --tasks "$$TASKS" --trials $(TRIALS) --model $(MODEL) --arms ecaa,claude-direct $(EVAL_ARGS)

eval-list-bbench: ## List cached BiomniBench-DA scenarios (id / category / difficulty / data-size) so the operator can pick TASKS for eval-tractable.
	@$(PYTHON) -m scripts.eval.list_bbench

eval-biomnibench: ## Tier 3 — only BiomniBench-DA (needs ECAA_EVAL_LIVE=1 + GEMINI_API_KEY + ECAA_ANTHROPIC_API_KEY)
	@$(PYTHON) -m scripts.eval.eval_runner biomnibench $(EVAL_ARGS)

eval-biomnibench-smoke: ## BiomniBench smoke (2 tasks, 1 trial)
	@$(PYTHON) -m scripts.eval.eval_runner biomnibench --smoke $(EVAL_ARGS)

eval-nekrutenko: ## Tier 4 — only Nekrutenko mtDNA eval (needs ECAA_EVAL_LIVE=1; add --error-matrix via EVAL_ARGS)
	@$(PYTHON) -m scripts.eval.eval_runner nekrutenko $(EVAL_ARGS)

eval-nekrutenko-smoke: ## Nekrutenko smoke (1 trial)
	@$(PYTHON) -m scripts.eval.eval_runner nekrutenko --smoke $(EVAL_ARGS)

eval-tests: ## Offline unit tests for the eval harness (no live API)
	@$(PYTHON) -m pytest scripts/eval/tests -q

eval-publish: ## Copy the redacted public scorecard from a run into docs/eval-results/ (non-gated). Usage: make eval-publish RUN=<run_dir>
	@test -n "$(RUN)" || { echo "usage: make eval-publish RUN=<run_dir>"; exit 2; }
	@$(PYTHON) -m scripts.eval.publish "$(RUN)"

schema-burden: ## Offline schema-authoring-burden analyzer -> docs/eval-results/schema-burden.{json,md} (non-gated)
	@$(PYTHON) -m scripts.eval.schema_burden

eval-campaign: ## Print the exact operator-gated commands for the committed campaign (does NOT run them)
	@echo "Campaign spec: scripts/eval/campaign.toml + docs/eval-results/CAMPAIGN.md"
	@echo ""
	@echo "OPERATOR-GATED commands (need ECAA_EVAL_LIVE=1 + GEMINI_API_KEY + ECAA_ANTHROPIC_API_KEY + AWS/harness authority):"
	@echo "  ECAA_EVAL_LIVE=1 $(PYTHON) -m scripts.eval.eval_runner nekrutenko --error-matrix --trials 10 --max-parallel $(NEK_PARALLEL)"
	@echo "  ECAA_EVAL_LIVE=1 $(PYTHON) -m scripts.eval.eval_runner biomnibench --trials 3 --max-parallel $(BBENCH_PARALLEL)"
	@echo ""
	@echo "Then (code-only):"
	@echo "  $(PYTHON) -m scripts.eval.verify_campaign <run_dir>"
	@echo "  make eval-publish RUN=<run_dir>"

eval-biomnibench-dryrun: ## BiomniBench dry-run smoke (--smoke flag; no live API needed beyond ECAA_EVAL_LIVE=1)
	@$(PYTHON) -m scripts.eval.eval_runner biomnibench --smoke --arms ecaa,claude-direct $(EVAL_ARGS)

eval-nekrutenko-dryrun: ## Nekrutenko dry-run smoke (--smoke flag; no live API needed beyond ECAA_EVAL_LIVE=1)
	@$(PYTHON) -m scripts.eval.eval_runner nekrutenko --smoke --arms ecaa,claude-direct $(EVAL_ARGS)

clean: ## Remove build artifacts
	cargo clean
	cd ui && rm -rf node_modules dist

doctor: ## Print toolchain readiness summary
	@echo "rustc: $$(rustc --version 2>/dev/null || echo 'MISSING')"
	@echo "cargo: $$(cargo --version 2>/dev/null || echo 'MISSING')"
	@echo "mold:  $$(mold --version 2>/dev/null || echo 'MISSING')"
	@echo "nextest: $$(cargo nextest --version 2>/dev/null | head -1 || echo 'MISSING (cargo install cargo-nextest)')"
	@echo "sccache: $$(sccache --version 2>/dev/null || echo 'MISSING (cargo install sccache; optional build cache)')"
	@echo "node:  $$(node --version 2>/dev/null || echo 'MISSING')"
	@echo "npm:   $$(npm --version 2>/dev/null || echo 'MISSING')"
	@echo "python:$$(python3 --version 2>/dev/null || echo 'MISSING')"
	@echo "extensions: $$(ecaa-workflow doctor-extensions 2>/dev/null || echo 'run after make install')"
