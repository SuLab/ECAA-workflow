//! Generic registry loader scaffolding. The five large YAML-dir
//! registries — `atom_registry`, `archetype_registry`,
//! `modality_registry`, `project_class_registry`, `gene_panel_registry` —
//! all reproduce the same ~50-line scaffold: walk a directory, filter
//! `_*.yaml` schema sidecars, deserialize each YAML, validate against an
//! embedded schema, sanity-check that the filename stem matches the
//! parsed `id`, and assemble a `BTreeMap<String, T>`.
//!
//! This module hosts the generic version; existing registries will
//! migrate onto it incrementally.

pub mod yaml_dir_registry;
