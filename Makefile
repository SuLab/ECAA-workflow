# ecaa-workflow Makefile — slim build/test surface for the open-source product.
#
# Conventions:
#   - All `cargo` calls go through default-members (the 4 binaries + ECAA validator).
#   - All `npm` calls run from ui/ (set by `cd ui &&` prefix).
#   - `make help` lists the canonical targets with one-line descriptions.

.PHONY: help build build-release install bootstrap test test-runner test-doc \
        test-fast test-core test-conversation test-harness test-server test-cli \
        test-ui lint-ui clippy fmt check types e2e e2e-playwright bench \
        bio-min dev-server dev-ui clean doctor

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
	bash scripts/build-bio-min.sh

# ── Test ─────────────────────────────────────────────────────────────────────

test: test-runner test-doc ## Run cargo test + doc tests across the workspace

test-runner: ## cargo test --workspace
	cargo test --workspace

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

# ── Lint / format / type-check ───────────────────────────────────────────────

fmt: ## cargo fmt --all
	cargo fmt --all

clippy: ## cargo clippy --workspace
	cargo clippy --workspace -- -D warnings

lint-ui: ## eslint over ui/src
	cd ui && npm run lint

check: test ## test + TypeScript noEmit
	cd ui && npx tsc --noEmit

types: ## Regenerate ts-rs TypeScript bindings into ui/src/types/
	cargo test -p ecaa-workflow-core export_bindings
	cargo test -p ecaa-workflow-conversation export_bindings

# ── End-to-end ───────────────────────────────────────────────────────────────

e2e: ## Quick smoke: build + emit + inspect a sample package
	bash scripts/test-e2e.sh

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

clean: ## Remove build artifacts
	cargo clean
	cd ui && rm -rf node_modules dist

doctor: ## Print toolchain readiness summary
	@echo "rustc: $$(rustc --version 2>/dev/null || echo 'MISSING')"
	@echo "cargo: $$(cargo --version 2>/dev/null || echo 'MISSING')"
	@echo "mold:  $$(mold --version 2>/dev/null || echo 'MISSING')"
	@echo "node:  $$(node --version 2>/dev/null || echo 'MISSING')"
	@echo "npm:   $$(npm --version 2>/dev/null || echo 'MISSING')"
	@echo "python:$$(python3 --version 2>/dev/null || echo 'MISSING')"
