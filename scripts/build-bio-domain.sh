#!/usr/bin/env bash
# Build a per-domain image that layers on top of bio-min.
# Usage: scripts/build-bio-domain.sh <domain> <tag>
#
# E.g. scripts/build-bio-domain.sh bio-gwas v0.1.0
# Produces ghcr.io/scripps/bio-gwas:v0.1.0 from containers/bio-gwas/Dockerfile.
#
# The script asserts bio-min is already built locally (its tag must
# resolve via `docker image inspect`). To bring up bio-min first:
#  scripts/build-bio-min.sh ghcr.io/scripps/bio-min:v0.1.0-eval

set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <domain> <tag>" >&2
  echo "e.g.   $0 bio-gwas v0.1.0" >&2
  exit 64
fi

DOMAIN="$1"
TAG="$2"
IMAGE="ghcr.io/scripps/${DOMAIN}:${TAG}"
CTX="containers/${DOMAIN}"
REPO_ROOT="$(git rev-parse --show-toplevel)"

if [[ ! -d "${REPO_ROOT}/${CTX}" ]]; then
  echo "[$DOMAIN] no Dockerfile context at ${CTX}" >&2
  exit 64
fi

# bio-min must be available locally. Default to the same tag the
# build-bio-min.sh script writes.
BIO_MIN_TAG="${BIO_MIN_TAG:-v0.1.0-eval}"
if ! docker image inspect "ghcr.io/scripps/bio-min:${BIO_MIN_TAG}" >/dev/null 2>&1; then
  echo "[$DOMAIN] bio-min not built locally; run scripts/build-bio-min.sh first" >&2
  exit 64
fi

cd "$REPO_ROOT"

echo "[$DOMAIN] building $IMAGE FROM bio-min:$BIO_MIN_TAG"
docker build \
  --build-arg "BIO_MIN_TAG=${BIO_MIN_TAG}" \
  --tag "$IMAGE" \
  --file "${CTX}/Dockerfile" \
  "$CTX"

DIGEST="$(docker image inspect --format='{{.Id}}' "$IMAGE")"
echo "[$DOMAIN] resolved digest: $DIGEST"
echo "[$DOMAIN] add to versions.lock under containers.extras.${DOMAIN//-/_}:"
echo "    image: \"ghcr.io/scripps/${DOMAIN}\""
echo "    digest: \"$DIGEST\""
