#!/usr/bin/env bash
#
# Walks a package root and finds amendment chains (linear parent → child
# → grandchild...). For any chain longer than --keep-last, prints /
# optionally deletes intermediate packages.
#
# Safety: defaults to --dry-run. --apply actually removes directories.
# Never deletes the oldest ancestor or the newest descendant — only
# intermediate nodes.

set -euo pipefail

ROOT="${ECAA_PACKAGE_ROOT:-$HOME/.scripps-workflow/packages}"
KEEP_LAST=3
MODE="dry-run"

usage() {
  cat <<EOF
Usage: $0 [--root <dir>] [--keep-last <N>] [--apply]

Walks <root> for package directories, reads each package's
policies/amendment-lineage.json, builds a parent→child lineage graph,
and prunes intermediate packages in any linear chain longer than
--keep-last.

Defaults:
  --root       \$ECAA_PACKAGE_ROOT (here: $ROOT)
  --keep-last  $KEEP_LAST
  dry-run mode (no deletions)

Flags:
  --apply      actually delete the pruned packages. Without this flag
               the script just prints a punch list of candidates.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --root)      ROOT="$2"; shift 2 ;;
    --keep-last) KEEP_LAST="$2"; shift 2 ;;
    --apply)     MODE="apply"; shift ;;
    -h|--help)   usage; exit 0 ;;
    *) echo "unknown flag: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ ! -d "$ROOT" ]]; then
  echo "prune-lineage: no package root at $ROOT" >&2
  exit 0
fi

command -v jq >/dev/null 2>&1 || {
  echo "prune-lineage: this script needs jq on PATH" >&2
  exit 1
}

# Collect every package and its parent reference.
# Output format: "<self_dir>\t<parent_package_id or empty>\t<mtime>"
declare -A parents    # child -> parent_id
declare -A by_id      # package_id -> dir

for pkg in "$ROOT"/*/; do
  pkg="${pkg%/}"   # strip trailing slash
  workflow_json="$pkg/WORKFLOW.json"
  [[ -f "$workflow_json" ]] || continue
  workflow_id=$(jq -r '.workflow_id // empty' "$workflow_json" 2>/dev/null || true)
  [[ -n "$workflow_id" ]] || continue
  by_id["$workflow_id"]="$pkg"

  lineage="$pkg/policies/amendment-lineage.json"
  if [[ -f "$lineage" ]]; then
    parent_id=$(jq -r '.parent.parent_package_id // empty' "$lineage" 2>/dev/null || true)
    if [[ -n "$parent_id" ]]; then
      parents["$workflow_id"]="$parent_id"
    fi
  fi
done

# Build each chain by walking from leaves (packages that nobody points to)
# back to the root via the parents map.
declare -A child_of    # parent_id -> child_id (assumes linear chains)
for child in "${!parents[@]}"; do
  parent="${parents[$child]}"
  child_of["$parent"]="$child"
done

chains=()

for pkg_id in "${!by_id[@]}"; do
  # Only walk from roots (packages with no parent entry)
  if [[ -n "${parents[$pkg_id]+x}" ]]; then
    continue
  fi
  # Walk down
  chain=("$pkg_id")
  cursor="$pkg_id"
  while [[ -n "${child_of[$cursor]+x}" ]]; do
    cursor="${child_of[$cursor]}"
    chain+=("$cursor")
  done
  if [[ ${#chain[@]} -gt 1 ]]; then
    chains+=("${chain[*]}")
  fi
done

if [[ ${#chains[@]} -eq 0 ]]; then
  echo "prune-lineage: no amendment chains found under $ROOT"
  exit 0
fi

echo "prune-lineage: scanning $ROOT (mode: $MODE, keep-last: $KEEP_LAST)"
echo

to_delete=()
for chain_str in "${chains[@]}"; do
  read -r -a chain <<< "$chain_str"
  n=${#chain[@]}
  if (( n <= KEEP_LAST )); then
    echo "chain of $n packages (within keep-last=$KEEP_LAST) — no pruning:"
    for id in "${chain[@]}"; do
      echo "    $id  -> ${by_id[$id]}"
    done
    continue
  fi
  echo "chain of $n packages — pruning $((n - KEEP_LAST)) intermediate node(s):"
  # Keep first 1 (the root) and last (KEEP_LAST - 1) descendants.
  keep_tail=$((KEEP_LAST - 1))
  for i in "${!chain[@]}"; do
    id="${chain[$i]}"
    dir="${by_id[$id]}"
    if (( i == 0 )) || (( i >= n - keep_tail )); then
      echo "  KEEP   $id  -> $dir"
    else
      echo "  PRUNE  $id  -> $dir"
      to_delete+=("$dir")
    fi
  done
  echo
done

if [[ "$MODE" == "dry-run" ]]; then
  echo "(dry-run — no packages deleted. Re-run with --apply to prune.)"
  exit 0
fi

echo "--apply set — deleting pruned packages..."
for dir in "${to_delete[@]}"; do
  echo "  rm -rf $dir"
  rm -rf -- "$dir"
done
echo "done."
