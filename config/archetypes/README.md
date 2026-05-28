# Archetypes — `config/archetypes/`

Plan §S6.7 / §S6.8 starter set. One archetype per YAML file, flat directory
layout. The directory is the source of truth for the deterministic
composer's fast-path matcher (plan §3.7 / §S6 / [DEC Q2.4]).

## What is an archetype?

An archetype is a thin composition file that names atoms by id and wires
them into a typed scaffold. It carries the slot mappings the composer's
slot-fill (S6.11) consumes, plus optional cross-archetype `compose:`
pointers for the Stage-6 fast-path matcher.

Per the plan modularity invariant ([CLAUDE.md §3.2 row "Archetype = thin
composition file"]):

> Archetypes hold scaffold + slot mappings only; no method-selection
> logic.

Method choices (which aligner, which DE method, etc.) live in the atom
layer's `discovery_*` runtime selection. Archetypes pre-select the
*atoms* to use; atoms then defer their concrete tool choice to the
agent at runtime via `discover_*`.

See `crates/core/src/archetype.rs::ArchetypeDefinition` for the Rust
type and `_archetype.schema.json` for the validating schema (per plan
§S6.7, schema-validate-before-deserialize discipline matching the atom
layer).

Each archetype YAML must:

- match `_archetype.schema.json` (regex-checked id + version + EDAM IRIs,
  closed `project_class` enum, `compose.position` enum),
- have a filename stem equal to its `id` (e.g., `single_cell_de.yaml`
  declares `id: single_cell_de`),
- reference `atom_id`s that resolve inside `config/stage-atoms/`
  (validated by the loader at composer boot — Phase 2),
- declare `goal_data` and optional `goal_format` as EDAM IRIs from the
  [EDAM ontology](https://edamontology.org), or `swfc:<slug>` for
  in-house extensions per ADR 0004.

## Layout

```
config/archetypes/
  _archetype.schema.json   # JSON Schema (Draft-07); will be embedded
                           # into the binary via include_str! when the
                           # loader lands (S6.8 follow-up).
  README.md                # this file.
  <archetype_id>.yaml      # one archetype per file. File stem must
                           # equal the `id` field.
```

Files prefixed with `_` are reserved for sidecars and shared fragments
and will be ignored by the loader.

## Current contents (S6.7 starter set, 12 archetypes)

| Project class | Archetype id | Modality |
|---|---|---|
| bioinformatics | `bulk_rnaseq_de` | Bulk RNA-seq differential expression |
| bioinformatics | `single_cell_de` | scRNA-seq clustering + DE |
| bioinformatics | `chip_seq_peaks` | ChIP-seq peak calling |
| bioinformatics | `variant_calling_germline` | Germline short-variant calling |
| bioinformatics | `gwas_coloc` | GWAS + colocalization |
| bioinformatics | `proteomics_dia` | Proteomics DIA |
| bioinformatics | `proteomics_dda` | Proteomics DDA (LFQ / TMT / SILAC) |
| bioinformatics | `metagenomics_taxonomic` | Metagenomics taxonomic profiling |
| bioinformatics | `spatial_transcriptomics` | Spatial Visium / Slide-seq / MERFISH |
| bioinformatics | `long_read_rnaseq` | Long-read ONT / PacBio Iso-Seq |
| clinical_trial | `clinical_trial_analysis` | Clinical-trial SAP analysis |
| time_series_forecast | `time_series_forecast` | Time-series forecasting |

These map 1:1 to the 12 stage-taxonomies under
`config/stage-taxonomies/`. The archetype layer is the future composer
input; the taxonomy layer is the legacy hand-authored input. The two
will run side-by-side until S6.10 cuts the composer over.

## Adding a new archetype

1. Pick a stable id matching `^[a-z][a-z0-9_]*[a-z0-9]$`. Prefer
   `<modality>_<analysis-type>` (e.g. `bulk_rnaseq_de`,
   `variant_calling_germline`).
2. Create `<id>.yaml`. Required fields: `id`, `version` (semver),
   `description`, `sme_summary`, `goal_data` (EDAM data IRI),
   `atoms` (non-empty list of `{atom_id, ...}`), and `project_class`.
3. Set `version` to `1.0.0` for the first cut. Bump on additive changes
   per [DEC QX.5] so existing sessions don't silently re-route.
4. Pick `goal_data` as the EDAM data class of the primary committed
   artifact (e.g. `data:0951` for a DE table, `data:3917` for a count
   matrix, `data:3498` for variant data). Set `goal_format` when the
   format is well-defined (e.g. `format:3475` tabular text,
   `format:3590` HDF5).
5. Populate `atoms` with `atom_id` references that exist in
   `config/stage-atoms/`. Cross-check with `ls config/stage-atoms/`
   before writing — the loader will reject unresolved ids.
6. Write `claim_boundary` as a one-sentence directive matching the
   modality. The text flows into the emitted package's
   `interpretation_policy`.
7. Set `project_class` to one of `bioinformatics | clinical_trial |
   time_series_forecast`. Match the closed enum in
   `crates/core/src/project_class.rs`.
8. Validate against the schema (Phase 2 will run schema validation in
   the core test suite; for now, hand-check against
   `_archetype.schema.json`).

## Conventions

- **Filename = id.** `single_cell_de.yaml` declares `id: single_cell_de`.
- **Lowercase + underscores.** Archetype ids are lowercase + underscores;
  no hyphens (so the id is a valid Rust / Python identifier when
  generated bindings need it).
- **Method neutrality.** Archetypes never name aligners, DE methods, or
  normalisation strategies. Those choices live in the atom layer's
  `discover_*` runtime selectors. An archetype's job is to pick *which
  atoms* run, not *how each atom runs*.
- **Slot-shape stability.** `slot_mappings` keys are the SME-facing
  intake field names; values are dotted paths into the atom inputs. Add
  new keys at the end of the map (BTreeMap serializes sorted, so order
  is canonical). Renames must bump the major version per [DEC QX.5].
- **Determinism.** Every collection field uses `BTreeMap` / `Vec`; YAML
  round-trips byte-identically. The future loader sorts by id before
  yielding.
- **Claim boundaries.** Every archetype that produces statistical
  comparisons, predictive labels, or interpretive annotations carries a
  `claim_boundary` directive. The text is the union of every
  participating atom's boundary plus the archetype-level addendum.

## Atom coverage gaps

Three of the 12 archetypes ride on a smaller atom set than they will
eventually need. The following atoms are referenced conceptually but
don't exist yet in `config/stage-atoms/`; they're the next batch for
S4.10 expansion:

- `cdisc_mapping` — clinical-trial CDISC ADaM/SDTM mapping
  (clinical_trial_analysis)
- `population_definition` — clinical-trial analysis-set assembly
  (clinical_trial_analysis)
- `endpoint_analysis` — clinical-trial pre-specified endpoint testing
  (clinical_trial_analysis)
- `time_series_decomposition` — trend / seasonality / residual
  (time_series_forecast)
- `time_series_feature_engineering` — lag / rolling / calendar features
  (time_series_forecast)
- `time_series_model_fitting` — ARIMA / state-space / neural fit
  (time_series_forecast)
- `time_series_forecasting` — point + interval forecasts
  (time_series_forecast)
- `spatial_domain_segmentation` — spatial-coordinate-aware clustering
  (spatial_transcriptomics)
- `spatially_variable_genes` — SVG discovery (spatial_transcriptomics)
- `isoform_discovery` — long-read transcript assembly
  (long_read_rnaseq)
- `differential_transcript_usage` — DTU testing (long_read_rnaseq)
- `long_read_benchmarking` — per-platform truth-set comparison
  (long_read_rnaseq)

Until those atoms ship, the affected archetypes are best-effort: they
list the closest-fit existing atom and the SME-named parameters flow
through `slot_mappings` so the agent can still execute the modality
under today's atom set.

## Cross-references

- Schema: `_archetype.schema.json` (this directory)
- Rust type: `crates/core/src/archetype.rs::ArchetypeDefinition`
- Plan: `docs/2026-04/unified-implementation-plan-2026-04-28.md` §S6.7 / §S6.8
- Atoms: `config/stage-atoms/` and its `README.md`
- ProjectClass enum: `crates/core/src/project_class.rs`
- EDAM audit: `docs/2026-04/edam-alignment-audit-2026-05-04.md`
