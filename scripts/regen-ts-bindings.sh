#!/usr/bin/env bash
# Regenerates every ts-rs binding in the workspace and syncs the result into
# `ui/src/types/` — the directory the UI actually imports and the directory
# `scripts/check-ts-bindings-fresh.sh` gates on.
#
# Why this is not just `cargo test export_bindings`:
#
#   1. ts-rs resolves its output directory at runtime from `TS_RS_EXPORT_DIR`,
#      falling back to `./bindings` relative to the test binary's CWD (the
#      crate manifest dir). With the variable unset, every crate writes to its
#      own `crates/<crate>/bindings/` and *nothing* ever lands in
#      `ui/src/types/`. A gate that diffs `ui/src/types/` after such a run can
#      never fire. We therefore set `TS_RS_EXPORT_DIR` explicitly per crate and
#      then copy into `ui/src/types/`.
#
#   2. ts-rs only ever *writes* files. A type that loses its `#[derive(TS)]`
#      leaves its `.ts` file behind forever. We wipe each per-crate sink before
#      regenerating so orphans disappear, and prune `ui/src/types/` to the
#      generated set (plus the hand-maintained `index.ts` barrel).
#
#   3. `ui/src/types/` is one flat namespace but the workspace is not: two
#      distinct Rust types can share a TS name. Silently letting the
#      last-written crate win would make the gate flap on crate ordering, so
#      collisions are resolved by the explicit pin table in
#      `canonical_owner()` below and anything unpinned is a hard error.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
ROOT="$PWD"
UI_TYPES="$ROOT/ui/src/types"

# Every crate holding at least one `#[ts(export)]` type, as
# `<cargo package>:<crates/ subdir>`. Verify with:
#   grep -rl 'ts(export' crates/*/src
CRATES=(
  "ecaa-workflow-types:ecaa-types"
  "ecaa-workflow-core:core"
  "ecaa-workflow-conversation:conversation"
  "ecaa-workflow-server:server"
  "ecaa-workflow-harness:harness"
)

# Canonical owner for a TS name defined by more than one Rust type.
#
# `BlockerContext` — `crates/ecaa-types/src/blocker.rs` wins over
# `crates/core/src/workflow_contracts/outcome.rs`. Reasons, in order:
#   * ecaa-types is the canonical binding for the ECAA v0.1 spec and the
#     JsonSchema drift source of truth; a spec type outranks a crate-internal
#     composer struct.
#   * The ecaa-types shape is what `SessionState::Blocked` and `BlockerEntry`
#     carry, i.e. what the UI reads on every session poll. The core struct
#     only reaches the wire through `ComposeOutcome`, which no UI module
#     imports (the Composition tab uses the hand-written
#     `ComposeOutcomePayload` in `ui/src/api/chatClient.ts`).
#   * It is also the shape already committed at `ui/src/types/BlockerContext.ts`,
#     so pinning it changes no UI behaviour.
# This pin is a workaround, not a resolution: `ui/src/types/ComposeOutcome.ts`
# still imports `./BlockerContext` while meaning the core struct. The real fix
# is a `#[ts(rename = "...")]`/`#[ts(export_to = "...")]` on the core struct so
# the two names stop colliding; until then this table keeps the gate honest and
# deterministic instead of ordering-dependent.
canonical_owner() {
  case "$1" in
    BlockerContext.ts) echo "ecaa-types" ;;
    *) echo "" ;;
  esac
}

echo "==> regenerating per-crate ts-rs sinks"
for entry in "${CRATES[@]}"; do
  pkg="${entry%%:*}"
  dir="${entry##*:}"
  sink="$ROOT/crates/$dir/bindings"
  rm -rf "$sink"
  mkdir -p "$sink"
  # `--lib` is deliberate: every `#[ts(export)]` site lives in `src/`, so the
  # generated tests are all in the lib target. Skipping the integration-test
  # targets keeps this target cheap.
  TS_RS_EXPORT_DIR="$sink" cargo test --quiet -p "$pkg" --lib export_bindings
  echo "    $pkg -> crates/$dir/bindings ($(find "$sink" -name '*.ts' | wc -l | tr -d ' ') files)"
done

echo "==> syncing into ui/src/types"
mkdir -p "$UI_TYPES"

# Union of generated basenames.
generated=$(for entry in "${CRATES[@]}"; do
  dir="${entry##*:}"
  find "$ROOT/crates/$dir/bindings" -maxdepth 1 -name '*.ts' -printf '%f\n'
done | sort -u)

collisions=0
for base in $generated; do
  # Which sinks produced this basename, and do they agree?
  owners=()
  for entry in "${CRATES[@]}"; do
    dir="${entry##*:}"
    [ -f "$ROOT/crates/$dir/bindings/$base" ] && owners+=("$dir")
  done

  distinct=()
  for d in "${owners[@]}"; do
    same=0
    for k in ${distinct[@]+"${distinct[@]}"}; do
      if cmp -s "$ROOT/crates/$d/bindings/$base" "$ROOT/crates/$k/bindings/$base"; then
        same=1
        break
      fi
    done
    [ "$same" -eq 0 ] && distinct+=("$d")
  done

  pick="${owners[0]}"
  if [ "${#distinct[@]}" -gt 1 ]; then
    pinned=$(canonical_owner "$base")
    if [ -z "$pinned" ]; then
      echo "ERROR: $base is generated with conflicting content by: ${distinct[*]}" >&2
      echo "       Two distinct Rust types share the TS name '${base%.ts}'." >&2
      echo "       Rename one (\`#[ts(rename = \"...\")]\`) or pin a canonical" >&2
      echo "       owner in canonical_owner() in $0 with a written rationale." >&2
      collisions=$((collisions + 1))
      continue
    fi
    echo "    WARN: $base collides across ${distinct[*]}; using pinned owner '$pinned'" >&2
    pick="$pinned"
  fi

  cp -f "$ROOT/crates/$pick/bindings/$base" "$UI_TYPES/$base"
done

if [ "$collisions" -gt 0 ]; then
  echo "ERROR: $collisions unresolved TS-name collision(s); ui/src/types not pruned." >&2
  exit 1
fi

# Prune bindings for types that no longer exist. `index.ts` is a
# hand-maintained barrel (its "Generated by ts-rs" header is inaccurate) and is
# never generated, so it is preserved.
pruned=0
while IFS= read -r f; do
  base=$(basename "$f")
  [ "$base" = "index.ts" ] && continue
  if ! printf '%s\n' $generated | grep -qxF "$base"; then
    rm -f "$f"
    echo "    pruned stale binding ui/src/types/$base"
    pruned=$((pruned + 1))
  fi
done < <(find "$UI_TYPES" -maxdepth 1 -name '*.ts')

# The barrel is hand-maintained, so it can name a type that no longer exists.
# That is a compile error in the UI, not a silent one, but flag it here too.
missing_barrel=0
while IFS= read -r name; do
  if [ ! -f "$UI_TYPES/$name.ts" ]; then
    echo "ERROR: ui/src/types/index.ts re-exports '$name' but $name.ts does not exist." >&2
    missing_barrel=$((missing_barrel + 1))
  fi
done < <(sed -n 's/^export type {[^}]*} from "\.\/\([A-Za-z0-9_]*\)";$/\1/p' "$UI_TYPES/index.ts")

if [ "$missing_barrel" -gt 0 ]; then
  echo "ERROR: $missing_barrel dangling re-export(s) in ui/src/types/index.ts." >&2
  exit 1
fi

echo "==> ui/src/types: $(find "$UI_TYPES" -maxdepth 1 -name '*.ts' | wc -l | tr -d ' ') files ($pruned pruned)"
