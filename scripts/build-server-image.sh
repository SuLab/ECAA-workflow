#!/usr/bin/env bash
# Build (and optionally push) the server OCI image.
#   build-server-image.sh <tag> [--push]
# Reproducibility: SOURCE_DATE_EPOCH from the commit; buildx rewrite-timestamp on push.
set -euo pipefail
TAG="${1:?usage: build-server-image.sh <tag> [--push]}"; shift || true
PUSH=""; [ "${1:-}" = "--push" ] && PUSH=1
SDE="$(git log -1 --pretty=%ct)"
export SOURCE_DATE_EPOCH="$SDE"
if [ -n "$PUSH" ]; then
  docker buildx inspect ecaa-multi >/dev/null 2>&1 || \
    docker buildx create --name ecaa-multi --driver docker-container --use
  docker buildx use ecaa-multi
  docker buildx build \
    --platform linux/amd64,linux/arm64 \
    --build-arg "SOURCE_DATE_EPOCH=$SDE" \
    --output "type=image,name=${TAG},push=true,rewrite-timestamp=true" \
    -t "$TAG" .
  echo "digest: $(docker buildx imagetools inspect "$TAG" | awk '/^Digest:/{print $2}')"
else
  DOCKER_BUILDKIT=1 docker build -t "$TAG" .
fi
