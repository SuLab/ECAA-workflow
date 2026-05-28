#!/usr/bin/env bash
set -euo pipefail
echo "scanning config/stage-atoms/ for deprecated preferred_container.network usage..."
found_stragglers=0
for f in config/stage-atoms/*.yaml; do
  if python3 -c "
import sys, yaml
try:
    data = yaml.safe_load(open('$f'))
    if data is None:
        sys.exit(0)
    c = data.get('preferred_container') or {}
    if 'network' in c:
        sys.exit(1)
except Exception as e:
    print(f'parse error in $f: {e}', file=sys.stderr)
    sys.exit(2)
"; then
    : # clean
  else
    rc=$?
    if [[ $rc -eq 1 ]]; then
      echo "STRAGGLER: $f (preferred_container.network present)"
      found_stragglers=1
    elif [[ $rc -eq 2 ]]; then
      echo "PARSE ERROR: $f"
      found_stragglers=1
    fi
  fi
done
if [[ $found_stragglers -ne 0 ]]; then
  echo "done. STRAGGLERS FOUND — migrate to safety.network."
  exit 1
fi
echo "done. no stragglers."
exit 0
