#!/usr/bin/env bash
# build-derived-image.sh — derived-image warm-up.
#
# Reads policies/runtime-prereqs.json + runtime/derived-image.Dockerfile
# from the package, computes the content hash via jq, then either:
#  - exits 0 immediately when an image with that tag is already in the
#  local docker registry (cache hit; cheap), OR
#  - invokes `docker buildx build` (or `docker build` if buildx absent)
#  to produce `<ECAA_DERIVED_IMAGE_TAG_PREFIX>:<hash>`.
#
# Writes runtime/derived-image.lock.json on success: { content_hash,
# base_image, modality, built_at, build_duration_secs }.
#
# Exit-code contract (must match crates/harness/src/executor/builder_exit_codes.rs):
#  10 — NOT_BUILDABLE: manifest empty / no install delta; harness falls back to host mode
#  20 — BUILD_FAILED: docker/jq error or image validation failed; harness errors out
#  30 — DOCKER_UNAVAILABLE: docker daemon unreachable / jq missing; operator infra issue
# When changing these, update both this file and builder_exit_codes.rs in lockstep.
#
# Exit codes (the harness pre-flight maps these to BlockerKind):
#  0 — success, image ready
#  10 — manifest empty / nothing to build (caller skips override)
#  20 — build failed
#  30 — docker daemon unreachable / docker not on PATH
#
# Usage:
#  scripts/build-derived-image.sh <package_dir>
#
# Inputs (env):
#  ECAA_DERIVED_IMAGE_TAG_PREFIX — image-tag prefix (default: scripps-derived)
#  ECAA_BUILDX_CACHE_DIR — buildkit cache root (default:
#  $ECAA_AGENT_CACHE_DIR/buildkit if
#  set, else ~/.ecaa-workflow/buildkit-cache)
#  ECAA_FORCE_IMAGE_REBUILD — set to 1 to skip the local-cache check
#  ECAA_IMAGE_BUILD_TIMEOUT_SECS — cap build wall time (default: 1800 = 30min)

set -euo pipefail

# ECR token-caching design (forward-compatible scaffold).
#
# When pull-from-ECR / push-to-ECR is wired into this script for the
# remote-AMI baking path, every per-task build currently re-runs
# `aws ecr get-login-password` — a ~300-800 ms round-trip per call.
# ECR tokens are valid for 12 h; refreshing 1 h early (TTL = 11 h)
# gives plenty of headroom against clock skew + multi-host fanout.
#
# Drop-in cache wrapper (uncomment + wire when ECR is introduced):
#
#   ECR_TOKEN_CACHE="/tmp/.ecr-token-${AWS_ACCOUNT_ID:-default}"
#   ECR_TOKEN_TTL_SECS=39600   # 11 hours
#   get_ecr_token() {
#       if [[ -f "$ECR_TOKEN_CACHE" ]]; then
#           local age
#           age=$(($(date +%s) - $(stat -c %Y "$ECR_TOKEN_CACHE")))
#           if [[ "$age" -lt "$ECR_TOKEN_TTL_SECS" ]]; then
#               cat "$ECR_TOKEN_CACHE"
#               return 0
#           fi
#       fi
#       # Refresh — write atomically via .tmp + mv so a concurrent
#       # reader never observes a partial token.
#       local tmp="${ECR_TOKEN_CACHE}.tmp.$$"
#       aws ecr get-login-password --region "${AWS_REGION:-us-west-2}" > "$tmp" || return 1
#       mv "$tmp" "$ECR_TOKEN_CACHE"
#       cat "$ECR_TOKEN_CACHE"
#   }
#
# Then replace inline `aws ecr get-login-password ...` invocations
# with `get_ecr_token`. Per-AWS_ACCOUNT_ID cache key ensures
# multi-account hosts don't cross-contaminate tokens.
#
# Mirror this design if/when ECR-token retrieval lands in Rust harness
# code: `crates/harness/src/main.rs` or wherever `aws ecr
# get-login-password` is invoked via `Command::new("aws")`. The Rust
# side should cache under the same /tmp/.ecr-token-<account> path so
# the script + binary share the cache when both pull on the same host.

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <package_dir>" >&2
  exit 30
fi

PACKAGE="$(realpath "$1")"
MANIFEST="$PACKAGE/policies/runtime-prereqs.json"
DOCKERFILE="$PACKAGE/runtime/derived-image.Dockerfile"
LOCK="$PACKAGE/runtime/derived-image.lock.json"

TAG_PREFIX="${ECAA_DERIVED_IMAGE_TAG_PREFIX:-scripps-derived}"
BUILD_TIMEOUT="${ECAA_IMAGE_BUILD_TIMEOUT_SECS:-1800}"
CACHE_DIR="${ECAA_BUILDX_CACHE_DIR:-${ECAA_AGENT_CACHE_DIR:-$HOME/.ecaa-workflow/agent-cache}/buildkit}"

# Serialize concurrent buildx invocations on the
# same host. Two parallel `docker buildx build` calls writing to the
# same `--cache-to=type=local,dest=$CACHE_DIR` corrupt the buildkit
# cache metadata (manifests overwrite each other mid-flush), forcing
# the next build to discard the entire cache and rebuild from scratch.
# `flock -n` is a non-blocking exclusive lock; on contention we fall
# through to the blocking `flock 200` so the second build waits for
# the first to finish rather than racing.
LOCK_FILE="${CACHE_DIR}/build.lock"
mkdir -p "$(dirname "$LOCK_FILE")"
exec 200>"$LOCK_FILE"
if ! flock -n 200; then
    echo "[derived-image] another build is in progress; waiting..." >&2
    flock 200
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "build-derived-image: docker not on PATH" >&2
  exit 30
fi
if ! docker info >/dev/null 2>&1; then
  echo "build-derived-image: docker daemon unreachable" >&2
  exit 30
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "build-derived-image: jq required to read the manifest; install jq" >&2
  exit 30
fi

if [[ ! -f "$MANIFEST" ]]; then
  echo "build-derived-image: manifest absent at $MANIFEST" >&2
  exit 10
fi

# Manifest is buildable iff it has a base_image AND at least one
# system or language package. Mirrors RuntimePrereqs::is_buildable in
# Rust.
BASE_IMAGE="$(jq -r '.base_image // empty' "$MANIFEST")"
HAS_PKGS="$(jq -r '
  (.system_packages.apt | length // 0) +
  (.system_packages.dnf | length // 0) +
  (.language_packages.r | length // 0) +
  (.language_packages.python | length // 0) +
  (.language_packages.conda | length // 0)
  > 0
' "$MANIFEST")"

if [[ -z "$BASE_IMAGE" || "$HAS_PKGS" != "true" ]]; then
  echo "build-derived-image: manifest is empty / not buildable — skipping" >&2
  exit 10
fi

if [[ ! -f "$DOCKERFILE" ]]; then
  echo "build-derived-image: Dockerfile absent at $DOCKERFILE" >&2
  echo "  (manifest is buildable but emitter did not write a Dockerfile?" >&2
  echo "   Re-emit the package or check derived_image::render_dockerfile)" >&2
  exit 20
fi

# Verify the install-proxy shim tree was copied alongside
# the Dockerfile. Without these files, the Dockerfile's COPY
# directives fail with an opaque docker error; check up front and
# point the operator at the emitter helper.
INSTALL_PROXY_DIR="$PACKAGE/runtime/install-proxy"
SHIM_FILES=(_common.py apt.py pip.py conda.py npm.py rscript.py gem.py)
MISSING_SHIMS=()
for shim in "${SHIM_FILES[@]}"; do
  if [[ ! -f "$INSTALL_PROXY_DIR/$shim" ]]; then
    MISSING_SHIMS+=("$shim")
  fi
done
if [[ ${#MISSING_SHIMS[@]} -gt 0 ]]; then
  echo "build-derived-image: install-proxy shims missing from $INSTALL_PROXY_DIR:" >&2
  for shim in "${MISSING_SHIMS[@]}"; do
    echo "  - $shim" >&2
  done
  echo "  (Re-emit the package; emitter.rs::copy_install_proxy should" >&2
  echo "   have copied the shims when the manifest is buildable.)" >&2
  exit 20
fi

# Content hash: SHA-256 of the manifest's serialized JSON. Mirrors
# Rust derived_image::content_hash. We use jq -c to canonicalize
# (compact, sorted keys) so host shells on different jq versions
# agree, then sha256sum the bytes.
#
# CRITICAL: The Rust side serializes via serde_json which writes keys
# in struct-declaration order, not alphabetical. To stay byte-stable
# across the Rust ↔ shell boundary we re-serialize Rust's exact
# byte-output by reading the file as-is. Shell hash:
HASH="$(sha256sum "$MANIFEST" | awk '{print $1}')"
TAG="${TAG_PREFIX}:${HASH}"

# Cache check: skip the build when the local docker registry already
# has this tag.
if [[ "${ECAA_FORCE_IMAGE_REBUILD:-0}" != "1" ]]; then
  if docker image inspect "$TAG" >/dev/null 2>&1; then
    echo "build-derived-image: cache hit for $TAG (local registry)"
    # Refresh the lock if missing so the harness can find it.
    if [[ ! -f "$LOCK" ]]; then
      MODALITY="$(jq -r '.modality // ""' "$MANIFEST")"
      jq -n \
        --arg hash "sha256:$HASH" \
        --arg base "$BASE_IMAGE" \
        --arg modality "$MODALITY" \
        --arg built_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        --argjson duration 0 \
        '{schema_version: 1, content_hash: $hash, base_image: $base, modality: $modality, built_at: $built_at, build_duration_secs: $duration, cache_hit: true}' \
        > "$LOCK"
    fi
    exit 0
  fi
fi

mkdir -p "$CACHE_DIR" 2>/dev/null || true

# Decide buildx vs plain docker build.
if docker buildx version >/dev/null 2>&1; then
  BUILDER=("docker" "buildx" "build" "--load")
  if [[ -d "$CACHE_DIR" ]]; then
    BUILDER+=(
      "--cache-from" "type=local,src=$CACHE_DIR"
      "--cache-to" "type=local,dest=$CACHE_DIR,mode=max"
    )
  fi
else
  BUILDER=("docker" "build")
fi

START="$(date +%s)"
echo "build-derived-image: building $TAG (timeout ${BUILD_TIMEOUT}s, base=$BASE_IMAGE)"

# `timeout` is on coreutils for any modern Linux; falls back gracefully
# when missing.
if command -v timeout >/dev/null 2>&1; then
  TIMEOUT_PREFIX=("timeout" "${BUILD_TIMEOUT}s")
else
  TIMEOUT_PREFIX=()
fi

if "${TIMEOUT_PREFIX[@]}" "${BUILDER[@]}" \
    -t "$TAG" \
    -f "$DOCKERFILE" \
    "$PACKAGE" >&2; then
  END="$(date +%s)"
  DURATION="$((END - START))"
  MODALITY="$(jq -r '.modality // ""' "$MANIFEST")"

  # Post-build smoke check: every shim must be at
  # /opt/ecaa-workflow/install-proxy/ and /usr/local/bin/<tool>
  # must be a symlink to its shim. Real binaries (when present in
  # the base) must have moved aside to /usr/local/bin/.real/. A
  # broken bake here would silently let an agent execute denied
  # installs at task time. Skipped when ECAA_SKIP_SHIM_SMOKE=1
  # (CI scenarios without a fresh docker daemon).
  if [[ "${ECAA_SKIP_SHIM_SMOKE:-0}" != "1" ]]; then
    if ! docker run --rm --entrypoint /bin/sh "$TAG" -c '
      set -eu
      for shim in _common.py apt.py pip.py conda.py npm.py rscript.py gem.py; do
        test -f "/opt/ecaa-workflow/install-proxy/$shim" || {
          echo "missing shim: /opt/ecaa-workflow/install-proxy/$shim" >&2
          exit 1
        }
      done
      for tool in apt apt-get pip pip3 conda mamba npm Rscript gem; do
        target="$(readlink -f "/usr/local/bin/$tool" 2>/dev/null || true)"
        case "$target" in
          /opt/ecaa-workflow/install-proxy/*.py) ;;
          *)
            echo "tool /usr/local/bin/$tool does not point at install-proxy (got: $target)" >&2
            exit 1
            ;;
        esac
      done
    ' >&2; then
      echo "build-derived-image: install-proxy smoke check failed for $TAG" >&2
      exit 20
    fi
  fi

  jq -n \
    --arg hash "sha256:$HASH" \
    --arg base "$BASE_IMAGE" \
    --arg modality "$MODALITY" \
    --arg built_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson duration "$DURATION" \
    '{schema_version: 1, content_hash: $hash, base_image: $base, modality: $modality, built_at: $built_at, build_duration_secs: $duration, cache_hit: false}' \
    > "$LOCK"
  echo "build-derived-image: built $TAG in ${DURATION}s"
  exit 0
else
  RC=$?
  if [[ $RC -eq 124 ]]; then
    echo "build-derived-image: build timed out after ${BUILD_TIMEOUT}s" >&2
  else
    echo "build-derived-image: docker build failed (exit $RC)" >&2
  fi
  exit 20
fi
