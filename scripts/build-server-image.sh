#!/usr/bin/env bash
# Build (and optionally push) the server OCI image.
#   build-server-image.sh <tag> [--push]
# Reproducibility: SOURCE_DATE_EPOCH from the commit; buildx rewrite-timestamp on push.
set -euo pipefail
TAG="${1:?usage: build-server-image.sh <tag> [--push]}"; shift || true
PUSH=""; [ "${1:-}" = "--push" ] && PUSH=1
SDE="$(git log -1 --pretty=%ct)"
export SOURCE_DATE_EPOCH="$SDE"
# Resolve the source commit on the HOST (where the git state is accurate) and
# thread it into the build as ECAA_SOURCE_COMMIT. The container build context
# omits .dockerignore'd-but-tracked files (.cargo/config.toml, docs/*), so an
# in-container `git status` would falsely report the tree dirty; passing the
# host-computed value keeps the baked provenance honest. Mirrors build.rs:
# `git rev-parse --short=12 HEAD` (+ `-dirty` when the tree has changes).
SRC_COMMIT="$(git rev-parse --short=12 HEAD 2>/dev/null || echo unknown)"
if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
  SRC_COMMIT="${SRC_COMMIT}-dirty"
fi
if [ -n "$PUSH" ]; then
  docker buildx inspect ecaa-multi >/dev/null 2>&1 || \
    docker buildx create --name ecaa-multi --driver docker-container --use
  docker buildx use ecaa-multi
  docker buildx build \
    --platform linux/amd64,linux/arm64 \
    --build-arg "SOURCE_DATE_EPOCH=$SDE" \
    --build-arg "ECAA_SOURCE_COMMIT=$SRC_COMMIT" \
    --output "type=image,name=${TAG},push=true,rewrite-timestamp=true" \
    -t "$TAG" .
  echo "digest: $(docker buildx imagetools inspect "$TAG" | awk '/^Digest:/{print $2}')"
else
  DOCKER_BUILDKIT=1 docker build \
    --build-arg "ECAA_SOURCE_COMMIT=$SRC_COMMIT" \
    -t "$TAG" .
fi
