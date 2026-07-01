#!/usr/bin/env bash
# Build an Apptainer SIF from the published bio-min OCI image (daemonless, login-node-safe).
#   build-sif.sh [<registry-ref>] [<out.sif>]
set -euo pipefail
REF="${1:-ghcr.io/scripps/bio-min:latest}"
OUT="${2:-bio-min.sif}"
command -v apptainer >/dev/null 2>&1 || { echo "apptainer not found (HPC login node)"; exit 1; }
apptainer build "$OUT" "docker://${REF}"
echo "built $OUT — run: apptainer exec --nv -B \"\$SCRATCH\" $OUT <cmd>"
