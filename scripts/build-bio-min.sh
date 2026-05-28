#!/usr/bin/env bash
# Build the bio-min container image and bump
# `crates/eval-adapters/versions.lock::containers.bio_min.digest` to
# the resolved sha256.
#
# Usage: scripts/build-bio-min.sh [TAG] [--push]
# Default tag: ghcr.io/scripps/bio-min:v0.1.0-eval
#
# Operator step (not in-session) — needs Docker installed and (for
# push) credentials configured.
#
# Cache strategy: this builder uses `docker buildx` with a bounded
# local cache at $ECAA_BUILDX_CACHE_DIR (default
# $ECAA_AGENT_CACHE_DIR/buildkit, else ~/.scripps-workflow/buildkit-cache).
# Same cache root as scripts/build-derived-image.sh so per-atom + base
# image builds share one bounded location instead of growing the
# unbounded default Docker daemon cache. Without this, a failed build
# can strand ~3 GB of partial layers across runs.

set -euo pipefail

TAG="${1:-ghcr.io/scripps/bio-min:v0.1.0-eval}"
CTX="containers/bio-min"
REPO_ROOT="$(git rev-parse --show-toplevel)"
LOCK_FILE="${REPO_ROOT}/crates/eval-adapters/versions.lock"

# Mirror scripts/build-derived-image.sh's CACHE_DIR resolution so both
# builders write to the same bounded location. The buildkit cache
# fronts the docker daemon layer cache for `--cache-from`/`--cache-to`
# local artifacts; operators can rm -rf the directory to reclaim
# space without breaking the daemon.
CACHE_DIR="${ECAA_BUILDX_CACHE_DIR:-${ECAA_AGENT_CACHE_DIR:-$HOME/.scripps-workflow/agent-cache}/buildkit}"

if ! command -v docker >/dev/null 2>&1; then
  echo "[bio-min] docker not found; install Docker or use the apptainer fallback" >&2
  exit 64
fi

cd "$REPO_ROOT"

mkdir -p "$CACHE_DIR" 2>/dev/null || true

echo "[bio-min] building $TAG from $CTX/Dockerfile"
echo "[bio-min] buildkit cache: $CACHE_DIR"

# Prefer buildx with bounded local cache; fall back to plain `docker
# build` only when buildx isn't installed (some bare-OCI hosts).
if docker buildx version >/dev/null 2>&1; then
  BUILDER=("docker" "buildx" "build" "--load")
  if [[ -d "$CACHE_DIR" ]]; then
    BUILDER+=(
      "--cache-from" "type=local,src=$CACHE_DIR"
      "--cache-to" "type=local,dest=$CACHE_DIR,mode=max"
    )
  fi
else
  echo "[bio-min] docker buildx unavailable — falling back to plain docker build (no bounded cache)" >&2
  BUILDER=("docker" "build")
fi

"${BUILDER[@]}" \
  --tag "$TAG" \
  --file "$CTX/Dockerfile" \
  "$CTX"

# Resolve digest. `docker image inspect` returns the local content
# digest; `docker manifest inspect` returns the registry-side digest
# (only available after push). We prefer the registry digest because
# that's what the harness pulls — but fall back to local for in-house
# builds that haven't been pushed yet.
DIGEST=""
if MANIFEST_JSON="$(docker manifest inspect "$TAG" 2>/dev/null)" \
    && [[ -n "$MANIFEST_JSON" ]]; then
  DIGEST="$(
    python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("config",{}).get("digest",""))' \
      <<<"$MANIFEST_JSON" 2>/dev/null || true
  )"
fi

if [[ -z "$DIGEST" || "$DIGEST" != sha256:* ]]; then
  REPO_DIGEST="$(docker image inspect --format='{{if .RepoDigests}}{{index .RepoDigests 0}}{{end}}' "$TAG" 2>/dev/null || true)"
  DIGEST="$(sed -nE 's|.*@(sha256:[a-f0-9]+).*|\1|p' <<<"$REPO_DIGEST" | head -n1)"
fi

if [[ -z "$DIGEST" || "$DIGEST" != sha256:* ]]; then
  # Local-only build, no registry digest yet. Use the image's own
  # sha256 from `Id`. This is only a stopgap — the harness pull will
  # fail until push completes.
  DIGEST="$(docker image inspect --format='{{.Id}}' "$TAG")"
fi

echo "[bio-min] resolved digest: $DIGEST"

# Bump versions.lock. Write a backup and use awk (portable across
# macOS BSD sed and GNU sed).
cp "$LOCK_FILE" "$LOCK_FILE.bak"
awk -v new_digest="$DIGEST" -v new_image="${TAG%:*}" '
  BEGIN { in_bio_min = 0 }
  /^containers:/ { print; next }
  /^  bio_min:/  { in_bio_min = 1; print; next }
  in_bio_min && /^    image:/ { print "    image: \"" new_image "\""; next }
  in_bio_min && /^    digest:/ { print "    digest: \"" new_digest "\""; in_bio_min = 0; next }
  /^[a-z]/ { in_bio_min = 0; print; next }
  { print }
' "$LOCK_FILE.bak" > "$LOCK_FILE"

echo "[bio-min] bumped $LOCK_FILE; review with:"
echo "  diff $LOCK_FILE.bak $LOCK_FILE"

# Optional cache size report so the operator can spot drift.
if [[ -d "$CACHE_DIR" ]]; then
  CACHE_BYTES="$(du -sb "$CACHE_DIR" 2>/dev/null | awk '{print $1}')"
  if [[ -n "$CACHE_BYTES" ]]; then
    CACHE_MB="$((CACHE_BYTES / 1024 / 1024))"
    echo "[bio-min] buildkit cache size: ${CACHE_MB} MB at $CACHE_DIR"
    echo "[bio-min]   manual prune: rm -rf \"$CACHE_DIR\""
    echo "[bio-min]   or:           docker buildx prune --keep-storage 10gb -f"
  fi
fi

# Optional push — skip unless --push is given.
if [[ "${2:-}" == "--push" ]]; then
  echo "[bio-min] pushing $TAG"
  docker push "$TAG"
fi
