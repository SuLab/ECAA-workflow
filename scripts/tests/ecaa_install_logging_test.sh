#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

mkdir -p "$SCRATCH/bin" "$SCRATCH/package"

cat > "$SCRATCH/bin/mamba" <<'SH'
#!/usr/bin/env bash
case "${1:-} ${2:-}" in
  "env list")
    printf 'Name Active Path\n  ecaa-bioc /cache/conda-envs/ecaa-bioc\n'
    ;;
  "list -n")
    printf 'bioconductor-deseq2 1.50.2 r45hdfd78af_0 bioconda\n'
    ;;
  *)
    exit 64
    ;;
esac
SH
chmod +x "$SCRATCH/bin/mamba"

PATH="$SCRATCH/bin:$PATH" \
ECAA_PACKAGE_DIR="$SCRATCH/package" \
ECAA_TASK_ID="normalisation" \
  "$REPO_ROOT/scripts/ecaa-install" bioc DESeq2 >/dev/null

LOG="$SCRATCH/package/runtime/install-log.jsonl"
TASK_LOG="$SCRATCH/package/runtime/outputs/normalisation/scripts/00_install.log"
test -s "$LOG"
test -s "$TASK_LOG"

jq -e '
  .atom_id == "normalisation"
  and .registry == "conda"
  and .package == "DESeq2"
  and .resolved_version == "1.50.2"
  and .source == "agent_runtime"
  and .action == "already_present"
' "$LOG" >/dev/null

grep -q 'ecaa-install conda DESeq2: version=1.50.2 action=already_present' "$TASK_LOG"
