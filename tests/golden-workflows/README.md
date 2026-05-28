# Golden-workflow corpus (Plan §S4.12 / §S4.13)

This directory pins the byte-level shape of every emitted package for
the canonical archetypes and edge cases. The CI gate `golden-diff`
runs the emit path against each `intake.yaml` and `git diff`s the
result against the committed `WORKFLOW.json`. Drift fails CI;
`scripts/regenerate-goldens.sh` is the only blessed path to bump.

## Layout

```
tests/golden-workflows/
├── archetypes/
│   ├── atac_seq_peaks/{intake.yaml, WORKFLOW.json}
│   ├── bulk_rnaseq_de/{intake.yaml, WORKFLOW.json}
│   ├── chip_seq_peaks/{intake.yaml, WORKFLOW.json}
│   ├── clinical_trial_analysis/{intake.yaml, WORKFLOW.json}
│   ├── gwas_coloc/{intake.yaml, WORKFLOW.json}
│   ├── long_read_rnaseq/{intake.yaml, WORKFLOW.json}
│   ├── metagenomics_taxonomic/{intake.yaml, WORKFLOW.json}
│   ├── proteomics_dda/{intake.yaml, WORKFLOW.json}
│   ├── proteomics_dia/{intake.yaml, WORKFLOW.json}
│   ├── single_cell_de/{intake.yaml, WORKFLOW.json}
│   ├── spatial_transcriptomics/{intake.yaml, WORKFLOW.json}
│   ├── time_series_forecast/{intake.yaml, WORKFLOW.json}
│   └── variant_calling_germline/{intake.yaml, WORKFLOW.json}
└── edge-cases/
    ├── per-sample-fan-out/{intake.yaml, WORKFLOW.json}
    ├── conditional-trimming/{intake.yaml, WORKFLOW.json}
    ├── sensitivity-comparison/{intake.yaml, WORKFLOW.json}
    ├── amendment-re-emit/{intake.yaml, WORKFLOW.json}
    └── cross-version-diff/{intake.yaml, WORKFLOW.json}
```

## Status (2026-05-04)

The directory tree and regenerate script ship in this commit; the per-archetype
`intake.yaml` files + their committed `WORKFLOW.json` snapshots populate
in a follow-up commit once the Stage 4 atom-extraction completion (S4.4 +
S4.11) lands and the composer fast path emits canonical shape under
`ECAA_COMPOSER=archetypes`. The `golden-diff` CI job therefore runs in
permissive mode (skips empty subdirectories with a warning) until the
corpus is fully populated.

## Regenerating

```bash
# All goldens:
bash scripts/regenerate-goldens.sh

# Just archetypes:
bash scripts/regenerate-goldens.sh archetypes

# Just edge cases:
bash scripts/regenerate-goldens.sh edge-cases

# A single archetype:
bash scripts/regenerate-goldens.sh single_cell_de
```

After regen, review `git diff tests/golden-workflows/` carefully — every
diff bytes should be intentional. Common reasons for an intended diff:

- New atom landed in the archetype (S4.10 bulk-extract, future composer
  Phase 2 / 3 edits, S5.7 method refresh)
- `WORKFLOW.json` schema bumped (e.g. new `Task::container` field per S15.2)
- A new policy file was emitted (e.g. CONSORT-AI checklist per S5.57)
- BCO `--emit-bco` toggled for `clinical_trial` archetype (S7.12)

Reasons a diff is **probably a regression**:

- `task_id` ordering shifted (composer determinism gate S7.15 should
  catch this — investigate before regenerating)
- Atom configuration values drifted (random or clock-derived; would
  defeat byte-reproducibility — track down the non-determinism)
- A new field appeared without a corresponding plan §S<N>.<M>
  reference in PR description

## CI gate

`.github/workflows/rust.yml::golden-diff` runs:

```yaml
- name: Verify golden-workflows are up to date
  run: |
    bash scripts/regenerate-goldens.sh
    git diff --exit-code tests/golden-workflows/ \
      || (echo "Goldens drifted. Run scripts/regenerate-goldens.sh and commit."; exit 1)
```

Permissive while corpus is populating: empty subdirectories skip with
`[regen] SKIP <name>: no intake.yaml at <path>`.

## Excluded files

The `runtime/intake-conversation.jsonl` and `runtime/decisions.jsonl`
artifacts are excluded from the byte-diff baseline (they include
timestamps + UUIDs by design — they're conversation provenance, not
DAG shape). See plan §3 architectural rule "Deterministic output".
