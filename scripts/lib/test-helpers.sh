# Shared pass/fail + JSON assertion helpers used by the `test-*.sh`
# drivers. Sourced via `source "$(dirname "$0")/lib/test-helpers.sh"`.
# Callers keep their own `PASS=0; FAIL=0` counters plus a single
# `OUT_DIR` global — the helpers read both.
#
# Contract:
#  - ok MSG / fail MSG print `[PASS]` / `[FAIL]` and bump counters
#  - assert_file RELATIVE_PATH verify OUT_DIR/RELATIVE_PATH exists
#  - assert_json_key FILE KEY verify OUT_DIR/FILE is JSON + has KEY
#  - assert_json_array_nonempty same, plus the value is a non-empty
#  list/dict
#  - assert_json_ld FILE verify OUT_DIR/FILE has @context and a
#  non-trivial @graph
#  - cleanup_trap CMD install CMD on EXIT+INT+TERM
#  - mktemp_scripps LABEL mktemp -d /tmp/scripps-LABEL-XXXXXX

ok() {
  echo "  [PASS] $1"
  PASS=$((PASS + 1))
}

fail() {
  echo "  [FAIL] $1"
  FAIL=$((FAIL + 1))
}

assert_file() {
  local f="${OUT_DIR:?OUT_DIR must be set before assert_file}/$1"
  if [[ -f "$f" ]]; then
    ok "file exists: $1"
  else
    fail "missing file: $1"
  fi
}

assert_json_key() {
  local f="${OUT_DIR:?OUT_DIR must be set before assert_json_key}/$1" key="$2"
  if python3 -c "import json,sys; d=json.load(open('$f')); sys.exit(0 if '$key' in d else 1)" 2>/dev/null; then
    ok "$1 has key '$key'"
  else
    fail "$1 missing key '$key'"
  fi
}

assert_json_array_nonempty() {
  local f="${OUT_DIR:?OUT_DIR must be set before assert_json_array_nonempty}/$1" key="$2"
  if python3 -c "
import json,sys
d=json.load(open('$f'))
v=d.get('$key')
sys.exit(0 if isinstance(v,(list,dict)) and len(v)>0 else 1)
" 2>/dev/null; then
    ok "$1[$key] is non-empty"
  else
    fail "$1[$key] is empty or missing"
  fi
}

assert_json_ld() {
  local f="${OUT_DIR:?OUT_DIR must be set before assert_json_ld}/$1"
  if python3 -c "
import json,sys
d=json.load(open('$f'))
ctx=d.get('@context','')
graph=d.get('@graph',[])
sys.exit(0 if ctx and len(graph)>2 else 1)
" 2>/dev/null; then
    ok "$1 is valid JSON-LD with @graph"
  else
    fail "$1 invalid JSON-LD (@context or @graph)"
  fi
}

cleanup_trap() {
  # Usage: cleanup_trap 'rm -rf "$OUT_DIR"'
  trap "$1" EXIT INT TERM
}

mktemp_scripps() {
  local label="${1:-test}"
  mktemp -d "/tmp/scripps-${label}-XXXXXX"
}
