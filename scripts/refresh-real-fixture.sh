#!/usr/bin/env bash
# Layer 2 fixture-staging helper for the ECAA emit-validation hardening plan.
#
# Drives the schema-clean roundtrip test in keep-output mode, captures the
# resulting tmpdir path, and copies the runtime/* sidecars plus
# ro-crate-metadata.json into
# `crates/ecaa-conformance/tests/fixtures/minimal-package-real/`.
#
# The destination is the conformance fixture tree's "real emit" counterpart
# to the hand-built `minimal-package/`. Once this fixture lands, the
# audit-proof corpus has a verified-clean reference point that the emit
# pipeline can be diffed against on every change.
#
# Usage:
#   bash scripts/refresh-real-fixture.sh
#
# The test asserts all 8 ECAA subgraph schemas pass against the keep-output
# package before copying — so a failing schema-validation surfaces here
# loudly, not silently as a broken fixture downstream.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DEST="$REPO_ROOT/crates/ecaa-conformance/tests/fixtures/minimal-package-real"

echo "[refresh-real-fixture] running emit roundtrip in keep-output mode..."
# Capture both stdout (for KEPT_PACKAGE_DIR) and stderr. `|| true` lets us
# print the output before checking exit; the post-check `grep` for the
# captured-path marker is the real failure signal.
TEST_OUTPUT="$(cargo test \
    -p ecaa-workflow-conversation \
    --test emit_roundtrip_schema_clean \
    emit_to_kept_dir_for_fixture_refresh \
    -- --test-threads=1 --nocapture --ignored 2>&1)" || true
echo "$TEST_OUTPUT"
if ! echo "$TEST_OUTPUT" | grep -qE 'test result: ok\.'; then
    echo "[refresh-real-fixture] FAIL: schema-clean roundtrip test did not pass" >&2
    exit 1
fi

# cargo prefixes the line with `test <name> ... ` before the test's own
# stdout; match the marker anywhere on the line then strip everything up
# to and including `KEPT_PACKAGE_DIR=`.
PKG_DIR="$(echo "$TEST_OUTPUT" | grep -E 'KEPT_PACKAGE_DIR=' | head -1 | sed 's/.*KEPT_PACKAGE_DIR=//')"
if [ -z "$PKG_DIR" ] || [ ! -d "$PKG_DIR" ]; then
    echo "[refresh-real-fixture] FAIL: could not locate KEPT_PACKAGE_DIR in test output" >&2
    exit 2
fi
echo "[refresh-real-fixture] captured: $PKG_DIR"

mkdir -p "$DEST"
# Clean before copy so removed sidecars don't linger in the fixture.
find "$DEST" -mindepth 1 -delete

cp -R "$PKG_DIR/." "$DEST/"
echo "[refresh-real-fixture] copied → $DEST"
ls -1 "$DEST/runtime" | head -30
