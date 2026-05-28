#!/usr/bin/env bash
# Validate each sub-graph JSON Schema syntax (draft-07).
set -euo pipefail

SPEC_DIR="$(cd "$(dirname "$0")/../../docs/ecaa-spec/subgraph-schemas" && pwd)"

python3 -c "import jsonschema" || pip install --user jsonschema

for schema in "$SPEC_DIR"/*.schema.json; do
    name="$(basename "$schema" .schema.json)"
    echo "=== $name ==="
    python3 -c "
import json
from jsonschema import Draft7Validator
schema = json.load(open('$schema'))
Draft7Validator.check_schema(schema)
print(f'  schema syntax OK ({len(schema)} top-level keys)')
"
done
