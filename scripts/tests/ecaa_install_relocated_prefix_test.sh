#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

mkdir -p "$SCRATCH/bin" "$SCRATCH/conda-envs/ecaa-bioc/conda-meta"
TEST_ENV="$SCRATCH/conda-envs/ecaa-bioc"
TEST_CALLS="$SCRATCH/mamba-calls.log"
printf '%s\n' "/previous/mount/conda-envs/ecaa-bioc" \
  > "$TEST_ENV/.ecaa-created-prefix"
printf 'legacy-prefix-bytes\n' > "$TEST_ENV/conda-meta/history"

cat > "$SCRATCH/bin/mamba" <<'SH'
#!/usr/bin/env bash
set -eu
printf '%s\n' "$*" >> "$TEST_CALLS"
if [ "${1:-} ${2:-}" = "env list" ]; then
  printf 'Name Active Path\n  ecaa-bioc %s\n' "$TEST_ENV"
  exit 0
fi
if [ "${1:-}" = "list" ] && [ "${2:-}" = "-p" ] && [ "${4:-}" = "--explicit" ]; then
  printf '@EXPLICIT\nhttps://conda.example/bioconductor-deseq2-1.50.2-0.conda#abc\n'
  exit 0
fi
if [ "${1:-} ${2:-}" = "create -y" ]; then
  prefix=""
  shift 2
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "-p" ]; then prefix="$2"; shift 2; continue; fi
    shift
  done
  test "$prefix" = "$TEST_ENV"
  mkdir -p "$prefix/conda-meta"
  printf 'rebuilt-from-explicit-lock\n' > "$prefix/conda-meta/history"
  exit 0
fi
if [ "${1:-} ${2:-}" = "list -n" ]; then
  printf 'bioconductor-deseq2 1.50.2 r45hdfd78af_0 bioconda\n'
  exit 0
fi
exit 64
SH
chmod +x "$SCRATCH/bin/mamba"

export TEST_ENV TEST_CALLS
PATH="$SCRATCH/bin:$PATH" \
CONDA_ENVS_DIRS="$SCRATCH/conda-envs" \
  "$REPO_ROOT/scripts/ecaa-install" bioc DESeq2 >/dev/null

test "$(cat "$TEST_ENV/.ecaa-created-prefix")" = "$TEST_ENV"
grep -q '^rebuilt-from-explicit-lock$' "$TEST_ENV/conda-meta/history"
grep -Fq "list -p $TEST_ENV --explicit" "$TEST_CALLS"
grep -Fq "create -y -p $TEST_ENV --file" "$TEST_CALLS"
test -z "$(find "$SCRATCH/conda-envs" -maxdepth 1 -name '.ecaa-bioc.pre-relocation.*' -print -quit)"
