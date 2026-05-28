# Stage atoms — `config/stage-atoms/`

Plan §S4.10 starter set. One atom per YAML file, flat directory layout.
The directory is the source of truth for the deterministic composer
(plan §3.7), gradually replacing the hand-authored taxonomies under
`config/stage-taxonomies/` as the composer wires up in S6+.

## What is an atom?

An atom is the unit of composition for the deterministic composer:
exactly one `(operation × input-type × output-type)` triple. Conceptually
closest to **Bazel subrules** — composable building blocks with explicit
interface contracts. See `crates/core/src/atom.rs::AtomDefinition` for
the Rust type and `_atom.schema.json` for the validating schema (per
plan §S4.3, schema-validate-before-deserialize discipline).

Each atom YAML must:

- match `_atom.schema.json` (regex-checked EDAM ids, closed `assignee`
  enum, role==discovery requires `discovery_kind`),
- have a filename stem equal to its `id` (e.g., `align_reads.yaml`
  declares `id: align_reads`),
- reference EDAM operation / data / format IRIs from the
  [EDAM ontology](https://edamontology.org) — or `swfc:<slug>` for
  in-house extensions per ADR 0004 (see `docs/2026-04/edam-alignment-audit-2026-05-04.md`),
- declare `depends_on` ids that resolve inside this directory (validated
  by `AtomRegistry::validate_consistency`).

## Layout

```
config/stage-atoms/
  _atom.schema.json     # JSON Schema (Draft-07); embedded into the
                        # binary via include_str!.
  README.md             # this file.
  <atom_id>.yaml        # one atom per file. File stem must equal the
                        # `id` field.
```

Files prefixed with `_` are ignored by the loader (reserved for sidecars
and shared fragments).

## Current contents (S4.10 starter set, 27 atoms)

| Category | Atom ids |
|---|---|
| Acquisition | `data_acquisition`, `data_import` |
| Read-level | `raw_qc`, `sequence_trimming`, `alignment`, `quantification` |
| Count-matrix | `qc_preprocessing`, `normalisation`, `batch_correction`, `integration`, `dimensionality_reduction` |
| Single-cell downstream | `clustering`, `cell_type_annotation` |
| Statistics | `differential_expression`, `pathway_enrichment` |
| ChIP-seq | `peak_calling` |
| Variant calling | `variant_calling`, `variant_filtering`, `variant_annotation` |
| GWAS / coloc | `gwas_summary_harmonization`, `colocalization` |
| Proteomics | `peptide_search`, `protein_quantification` |
| Metagenomics | `taxonomic_classification`, `diversity_analysis` |
| Reporting | `reporting`, `final_reporting` |

This is a **starter set** intended to cover the most common
cross-taxonomy stages. The full extraction (175 stage rows × 12
taxonomies) is plan §S4.11 territory.

## Validating

Schema + consistency validation runs as part of the core unit-test
suite:

```sh
cargo test -p scripps-workflow-core --lib atom_registry
```

The test fixtures use tempdirs, so the suite covers correctness of the
loader. To validate the live directory, the composer will load it via
`AtomRegistry::load_from_dir(Path::new("config/stage-atoms"))` at
emit-time and surface schema errors with file + line context.

## Conventions

- **Filename = id.** `align_reads.yaml` declares `id: align_reads`.
- **Verb-noun ids.** Atom ids are lowercase + underscores, conventionally
  `<verb>_<noun>` (e.g. `align_reads`, `quantify_features`,
  `discover_normalisation_method`).
- **Method neutrality.** Operation atoms list `attributes.candidate_tools`
  rather than picking a winner. Method selection is delegated to the
  agent at runtime via `discover_*` siblings.
- **Determinism.** Every collection field uses `BTreeMap` / `Vec`; YAML
  round-trips byte-identically. The atom_registry loader sorts by id
  before yielding.
- **EDAM extensions.** When EDAM upstream lacks coverage, use
  `swfc:<slug>` per ADR 0004. The audit doc
  (`docs/2026-04/edam-alignment-audit-2026-05-04.md`) enumerates the
  current `swfc:` namespace and tracks quarterly upstream-PR cadence.
- **Claim boundaries.** Atoms whose outputs invite over-claiming
  (DE, enrichment, cell-type calls, variant pathogenicity) carry a
  `claim_boundary` directive that the LLM restates during confirmation
  and that flows into the emitted package's `interpretation_policy`.

## Cross-references

- Schema: `_atom.schema.json` (this directory)
- Rust type: `crates/core/src/atom.rs::AtomDefinition`
- Loader / registry: `crates/core/src/atom_registry.rs`
- EDAM audit: `docs/2026-04/edam-alignment-audit-2026-05-04.md`
- Plan: `docs/2026-04/unified-implementation-plan-2026-04-28.md` §S4.10
