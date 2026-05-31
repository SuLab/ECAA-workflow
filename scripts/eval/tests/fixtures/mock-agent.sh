#!/usr/bin/env bash
# Minimal harness-contract agent: marks the first ready task complete.
set -euo pipefail
PKG="$1"
TID="$(python3 -c "import json,sys;d=json.load(open(sys.argv[1]+'/WORKFLOW.json'));print(next(t['id'] for t in d['tasks'] if t.get('state')=='ready'))" "$PKG")"
mkdir -p "$PKG/runtime/outputs/$TID"
echo "# narrative for $TID" > "$PKG/runtime/outputs/$TID/report.md"
printf '{"task_id":"%s","new_state":"completed"}\n' "$TID" \
  > "$PKG/runtime/outputs/$TID/state.patch.json"
