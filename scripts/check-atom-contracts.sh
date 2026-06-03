#!/usr/bin/env bash
# Per-atom contract lint: every catalog atom round-trips serde, resolves its
# edam_data output through the plot-affordance registry (or is figure_exempt),
# and has non-orphan depends_on. Shells the crates/core integration test so
# the lint shares one implementation with the test suite.
set -euo pipefail
echo "[lint] atom contracts"
cargo test -p ecaa-workflow-core --test atom_registry -- atom_contract_lint --nocapture
