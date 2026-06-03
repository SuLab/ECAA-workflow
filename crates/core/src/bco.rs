//! IEEE 2791-2020 Biocompute Object emit (opt-in).
//!
//! Emits a load-bearing subset of the IEEE 2791-2020 Biocompute Object
//! schema as `bco.json` at the package root. Hand-rolled JSON shape —
//! we don't carry the full IEEE 2791 schema validator; only the fields
//! the plan references for cross-referencing composer rationale +
//! provenance. The full schema is available at
//! <https://docs.biocomputeobject.org/object/> for SMEs who want to
//! validate the emitted document with an external tool.
//!
//! # Schema fields written
//!
//! - `object_id` — stable id derived from
//!   `composition.matched_archetype + intake.modality + sha256 prefix`
//! - `etag` — sha256 of the canonical JSON serialization with the
//!   `etag` *field removed* (R4-Sci-4 fix; previously the field was
//!   zeroed in place, which left the property name + empty string in
//!   the hash input — a tiny but real distinction the IEEE 2791
//!   reference implementation calls out).
//! - `provenance_domain` — composition.matched_archetype +
//!   composition.rationale, plus IEEE 2791-mandated `created`/`modified`
//!   timestamps. These are DETERMINISTIC: `build_bco` derives a frozen
//!   emit-time from the SHA-256 of the BCO's stable content (the same
//!   `clock::deterministic_emit_time` plumbing the RO-Crate emit path
//!   uses via `frozen_clock_from_intake`), so two emits of identical
//!   inputs produce byte-identical timestamps and `etag`. Callers that
//!   want to thread the emitter's `FrozenClock` use `build_bco_with_clock`;
//!   tests pin an explicit string via `build_bco_at`.
//! - `usability_domain` — one-line goal description.
//! - `description_domain` — atoms list, mirroring the
//!   `pipeline_steps` shape from the IEEE 2791 spec.
//! - `execution_domain` — compute profile reference (placeholder
//!   pointer; full sizing lives in the package's
//!   `policies/intake-facts.json`).
//! - `io_domain` — R4-Sci-4. `input_subdomain` lists each atom's
//!   declared inputs (port name + semantic type IRI); `output_subdomain`
//!   lists each atom's declared outputs. IEEE 2791-2020 §3.7.
//! - `parametric_domain` — R4-Sci-4. Flattens every atom's
//!   `attributes` map into parametric entries
//!   (`{ "param": "...", "value": <json>, "step": <atom_id> }`).
//!   IEEE 2791-2020 §3.8.
//! - `error_domain` — R4-Sci-4. Placeholder with empty
//!   `empirical_error` and `algorithmic_error` arrays; SMEs fill these
//!   in post-emit from result.json metrics. IEEE 2791-2020 §3.9.
//!
//! # Why opt-in
//!
//! Per the plan, BCO emit is gated by `--emit-bco` (forthcoming flag
//! work — separate commit). This module ships the function +
//! roundtrip test only; no wiring into the main emit pipeline yet.

use crate::composer::CompositionResult;
use crate::intake_facts::IntakeFacts;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Emit a Biocompute Object document for `composition`
/// to `<output_dir>/bco.json`.
///
/// Determinism: the generated JSON is sorted-by-key (BTreeMap-style
/// via `serde_json::Value` object insertion) so identical inputs hash
/// to identical etags. The `created`/`modified` timestamps are derived
/// deterministically from the BCO's stable content (via
/// [`clock::deterministic_emit_time`](crate::clock::deterministic_emit_time))
/// rather than wall-clock — so the whole document, including the `etag`
/// that hashes over it, is byte-reproducible across re-emits of the same
/// inputs.
pub fn emit_bco(
    composition: &CompositionResult,
    intake: &IntakeFacts,
    output_dir: &Path,
) -> Result<()> {
    let bco = build_bco(composition, intake);
    let bytes = serde_json::to_vec_pretty(&bco).context("serializing Biocompute Object to JSON")?;
    let path = output_dir.join("bco.json");
    std::fs::write(&path, bytes)
        .with_context(|| format!("writing Biocompute Object to {}", path.display()))?;
    Ok(())
}

/// Build the in-memory `serde_json::Value` representation. Split out
/// from `emit_bco` so the round-trip test can assert structure
/// without touching disk.
///
/// Determinism: the `created`/`modified` timestamps are derived from a
/// SHA-256 over the BCO's stable content (archetype id, modality, goal,
/// rationale) routed through [`crate::clock::deterministic_emit_time`] —
/// the same fold the RO-Crate emit path uses in `frozen_clock_from_intake`.
/// Two builds with identical inputs therefore produce byte-identical JSON,
/// including the content-addressed `etag`. Callers that want to reuse the
/// emitter's `FrozenClock` instead use [`build_bco_with_clock`]; tests pin
/// an explicit string via [`build_bco_at`].
pub fn build_bco(composition: &CompositionResult, intake: &IntakeFacts) -> Value {
    let clock = deterministic_clock_for(composition, intake);
    build_bco_with_clock(composition, intake, &clock)
}

/// Build a BCO using an explicit [`Clock`](crate::clock::Clock). The
/// emit pipeline derives a `FrozenClock` from the intake hash
/// (`frozen_clock_from_intake`); passing that same clock here keeps the
/// BCO's `created`/`modified` aligned with the package's RO-Crate
/// `dateCreated` for a given intake.
pub fn build_bco_with_clock(
    composition: &CompositionResult,
    intake: &IntakeFacts,
    clock: &dyn crate::clock::Clock,
) -> Value {
    build_bco_at(composition, intake, &clock.now_rfc3339())
}

/// Derive a deterministic `FrozenClock` from the BCO's stable content so
/// `build_bco` is byte-reproducible without any wall-clock read. Mirrors
/// `emitter::frozen_clock_from_intake`: SHA-256 of the content → folded
/// through `deterministic_emit_time` into a 2026..2076 RFC-3339 value
/// that's schema-indistinguishable from a real emit timestamp.
fn deterministic_clock_for(
    composition: &CompositionResult,
    intake: &IntakeFacts,
) -> crate::clock::FrozenClock {
    use sha2::{Digest, Sha256};
    let archetype_id = composition
        .matched_archetype
        .as_deref()
        .unwrap_or("backward-chain");
    let seed = format!(
        "{}|{}|{}|{}",
        archetype_id, intake.modality, composition.goal.edam_data, composition.rationale
    );
    let digest = Sha256::digest(seed.as_bytes());
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&digest);
    crate::clock::FrozenClock {
        at: crate::clock::deterministic_emit_time(&hash),
    }
}

/// Build a BCO with an explicit RFC-3339 `created_at` string for the
/// `provenance_domain.created` and `.modified` fields. The
/// `created_at` value is used for both because a fresh emit is, by
/// definition, also the last modification.
///
/// Lowest-level builder: accepts the timestamp as a parameter. The
/// production path ([`build_bco`]) supplies a deterministic value derived
/// from content via [`deterministic_clock_for`]; tests pin a constant
/// here directly.
pub fn build_bco_at(
    composition: &CompositionResult,
    intake: &IntakeFacts,
    created_at: &str,
) -> Value {
    // object_id: derived from composition + intake. Keeps the value
    // stable across re-emits of the same composition (deterministic
    // contract).
    let archetype_id = composition
        .matched_archetype
        .as_deref()
        .unwrap_or("backward-chain");
    let object_id = format!(
        "urn:ecaax:bco:{}:{}:{}",
        intake.modality,
        archetype_id,
        sha256_prefix(&format!(
            "{}|{}|{}",
            archetype_id, intake.modality, composition.goal.edam_data
        ))
    );

    // provenance_domain — IEEE 2791-2020 §3.2. R4-Sci-4 — `created`
    // and `modified` are populated via the caller-supplied RFC-3339
    // string (`now_rfc3339()` in production; fixture-stable values in
    // tests).
    let provenance_domain = json!({
        "name": format!("ECAA-workflow composition {}", archetype_id),
        "version": "1.0.0",
        "license": "CC-BY-4.0",
        "created": created_at,
        "modified": created_at,
        "contributors": [],
        "review": [],
        "derived_from": null,
        "obsolete_after": null,
        "embargo": {},
        "matched_archetype": archetype_id,
        "rationale": composition.rationale,
    });

    // usability_domain — IEEE 2791-2020 §3.3. One-line goal
    // description; SMEs reading the BCO see this first.
    let usability_text = match &composition.goal.edam_format {
        Some(f) => format!(
            "Produce {} ({}) per goal extracted from intake.",
            composition.goal.edam_data, f
        ),
        None => format!(
            "Produce {} per goal extracted from intake.",
            composition.goal.edam_data
        ),
    };
    let usability_domain = json!([usability_text]);

    // description_domain — IEEE 2791-2020 §3.4 pipeline_steps.
    // One step per composed atom; ordering preserves the
    // Composition's emission order. when an atom's
    // resolved container carries a digest, expose it on the step
    // (PCCP-friendly per Round-4 §22.18) so the BCO consumer can
    // tie a pipeline step to its supply-chain artifact without
    // re-resolving the registry.
    let pipeline_steps: Vec<Value> = composition
        .atoms
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let prerequisites: Vec<Value> =
                c.depends_on.iter().map(|d| json!({"name": d})).collect();
            let mut step = json!({
                "step_number": i,
                "name": c.stage_id,
                "description": c.atom.description,
                "prerequisite": prerequisites,
                "input_list": [],
                "output_list": [],
                "edam_operation": c.atom.edam_operation,
                "edam_data": c.atom.edam_data,
                "edam_format": c.atom.edam_format,
            });
            if let Some(container) = c.container.as_ref() {
                if let Some(obj) = step.as_object_mut() {
                    obj.insert(
                        "container_image".into(),
                        json!(format!("{}:{}", container.image, container.tag)),
                    );
                    if !container.digest.is_empty() {
                        obj.insert("container_digest".into(), json!(container.digest));
                    }
                }
            }
            step
        })
        .collect();
    let description_domain = json!({
        "keywords": [intake.modality.clone(), archetype_id.to_string()],
        "platform": ["ecaa-workflow"],
        "pipeline_steps": pipeline_steps,
    });

    // execution_domain — IEEE 2791-2020 §3.5. Pointer-only for the
    // detailed compute profile; container digests + cgroup envelope
    // populate the IEEE 2791 fields we have a 1:1 mapping for.
    // `environment_variables` carries
    // `SCRIPPS_DEFAULT_CONTAINER_IMAGE` + `SCRIPPS_DEFAULT_CONTAINER_DIGEST`
    // when a package-wide default container is set; per-step
    // overrides land on the corresponding pipeline_step entry. The
    // PCCP `change_record` extension (Round-4 §22.18) is a forward-
    // looking placeholder until the real change-tracking ledger lands
    // alongside an emitted-package amendment chain.
    let methods: Vec<Value> = intake.methods.iter().map(|m| json!(m)).collect();
    let mut env_vars = serde_json::Map::new();
    let default_container = composition
        .atoms
        .iter()
        .find_map(|c| c.container.as_ref())
        .cloned();
    if let Some(container) = default_container.as_ref() {
        env_vars.insert(
            "SCRIPPS_DEFAULT_CONTAINER_IMAGE".into(),
            json!(format!("{}:{}", container.image, container.tag)),
        );
        if !container.digest.is_empty() {
            env_vars.insert(
                "SCRIPPS_DEFAULT_CONTAINER_DIGEST".into(),
                json!(container.digest),
            );
        }
    }
    let execution_domain = json!({
        "script": [],
        "script_driver": "ecaa-workflow-harness",
        "software_prerequisites": methods,
        "external_data_endpoints": [],
        "environment_variables": Value::Object(env_vars),
        "compute_profile_ref": "policies/intake-facts.json",
        // PCCP-friendly extension. Empty until the
        // amendment ledger feeds a non-empty change_record list at
        // emit time. WRROC Tier-3 emit (S6.14) carries the same
        // information via `prov:wasGeneratedBy::ParameterConnection`
        // for non-clinical archetypes.
        "extension_domain": [
            {
                "extension_schema": "https://ecaa-workflow/bco-extensions/pccp-change-record/v1",
                "change_records": []
            }
        ],
    });

    // R4-Sci-4 — IEEE 2791-2020 §3.7 `io_domain`. Two subdomains:
    // `input_subdomain` enumerates the cross-pipeline declared inputs
    // (one entry per (atom, input-port) pair), `output_subdomain`
    // enumerates declared outputs. The minimal IEEE 2791 shape is
    // a `uri`-keyed object per entry — we add `name`, `semantic_type`,
    // and `step` so the document is self-describing without a side
    // lookup against the description_domain.
    let input_subdomain: Vec<Value> = composition
        .atoms
        .iter()
        .flat_map(|c| {
            let stage_id = c.stage_id.clone();
            c.atom.inputs.iter().map(move |port| {
                let st_id = port.semantic_type.stable_id();
                json!({
                    "uri": {
                        "uri": st_id.clone(),
                        "filename": port.name,
                    },
                    "step": stage_id,
                    "semantic_type": st_id,
                })
            })
        })
        .collect();
    let output_subdomain: Vec<Value> = composition
        .atoms
        .iter()
        .flat_map(|c| {
            let stage_id = c.stage_id.clone();
            c.atom.outputs.iter().map(move |port| {
                let st_id = port.semantic_type.stable_id();
                json!({
                    "uri": {
                        "uri": st_id.clone(),
                        "filename": port.name,
                    },
                    "step": stage_id,
                    "semantic_type": st_id,
                })
            })
        })
        .collect();
    let io_domain = json!({
        "input_subdomain": input_subdomain,
        "output_subdomain": output_subdomain,
    });

    // R4-Sci-4 — IEEE 2791-2020 §3.8 `parametric_domain`. Flattens
    // every atom's `attributes` map (`BTreeMap<String, serde_json::Value>`)
    // into parametric entries keyed by the atom's stage_id. BTreeMap
    // iteration order is sorted-by-key so the emission is deterministic
    // for the byte-reproducibility contract.
    let parametric_domain: Vec<Value> = composition
        .atoms
        .iter()
        .flat_map(|c| {
            let stage_id = c.stage_id.clone();
            c.atom.attributes.iter().map(move |(k, v)| {
                json!({
                    "param": k,
                    "value": v,
                    "step": stage_id,
                })
            })
        })
        .collect();

    // R4-Sci-4 — IEEE 2791-2020 §3.9 `error_domain`. Placeholder with
    // both empirical and algorithmic error arrays empty; consumers
    // fill them in after running the package and observing
    // result.json metrics. The schema mandates the field shape; the
    // content is forward-looking.
    let error_domain = json!({
        "empirical_error": [],
        "algorithmic_error": [],
    });

    // Assemble the BCO sans etag, then compute the etag over the
    // canonical bytes and stitch it in. The etag is computed over
    // the document with the `etag` *field removed* (not present at
    // all in the hash input), matching the IEEE 2791-2020 reference
    // behaviour. Emitting the field as `""` before hashing would
    // leave the `"etag":""` property name + value in the hash input.
    let mut doc = json!({
        "object_id": object_id,
        "spec_version": "https://w3id.org/ieee/ieee-2791-schema/2791object.json",
        "provenance_domain": provenance_domain,
        "usability_domain": usability_domain,
        "description_domain": description_domain,
        "execution_domain": execution_domain,
        "io_domain": io_domain,
        "parametric_domain": parametric_domain,
        "error_domain": error_domain,
    });

    let canonical_bytes = serde_json::to_vec(&doc).expect("BCO json must serialize");
    let etag = crate::hash_utils::sha256_hex(&canonical_bytes);
    if let Some(obj) = doc.as_object_mut() {
        obj.insert("etag".into(), json!(etag));
    }
    doc
}

fn sha256_prefix(s: &str) -> String {
    crate::hash_utils::sha256_short(s.as_bytes(), 16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::{AtomAssignee, AtomDefinition, AtomRole};
    use crate::composer::ComposedAtom;
    use crate::goal_spec::GoalSpec;
    use crate::project_class::ProjectClass;
    use std::collections::BTreeMap;

    fn sample_atom() -> AtomDefinition {
        AtomDefinition {
            id: "differential_expression".into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            confirmatory: false,
            discovery_kind: None,
            description: "Test for DEGs.".into(),
            edam_operation: "operation:3223".into(),
            edam_data: Some("data:0951".into()),
            edam_format: Some("format:3475".into()),
            assignee: AtomAssignee::Agent,
            depends_on: vec!["normalisation".into()],
            excludes: vec![],
            attributes: BTreeMap::new(),
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: None,
            resource_profile: None,
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            required_artifacts: vec![],
            validators: vec![],
            runtime_packages: Default::default(),
            parameters: Vec::new(),
            provenance: None,
            estimated_duration: None,
            safety: Default::default(),
        }
    }

    fn sample_composition() -> CompositionResult {
        let atom = sample_atom();
        let depends = atom.depends_on.clone();
        CompositionResult {
            matched_archetype: Some("bulk_rnaseq_de".into()),
            match_score: 6,
            atoms: vec![ComposedAtom {
                stage_id: "differential_expression".into(),
                atom,
                depends_on: depends,
                required: true,
                bindings: Vec::new(),
                container: None,
            }],
            goal: GoalSpec {
                edam_data: "data:0951".into(),
                edam_format: Some("format:3475".into()),
                modifiers: BTreeMap::new(),
                source_prose: None,
                confidence: 0.9,
            },
            rationale: "Compare bulk RNA-seq across two conditions.".into(),
            atom_rationales: BTreeMap::new(),
            resource_estimate: crate::composer::ResourceEstimate::default(),
        }
    }

    fn sample_intake() -> IntakeFacts {
        IntakeFacts {
            modality: "bulk_rnaseq".into(),
            project_class: ProjectClass::Bioinformatics,
            organism_taxon_id: Some(9606),
            organism_name: Some("Homo sapiens".into()),
            methods: vec!["deseq2".into()],
            sample_count: Some(12),
            coverage_depth: Some(30),
            cell_count: None,
            database_size_gb: None,
            pinned_accessions: Vec::new(),
            pinned_reference_bundles: Vec::new(),
            literature_review_requested: false,
            excluded_atoms: Vec::new(),
        }
    }

    #[test]
    fn build_bco_populates_load_bearing_fields() {
        let composition = sample_composition();
        let intake = sample_intake();
        let bco = build_bco(&composition, &intake);
        assert!(bco.get("object_id").is_some());
        assert!(bco.get("etag").is_some());
        assert!(bco.get("provenance_domain").is_some());
        assert!(bco.get("usability_domain").is_some());
        assert!(bco.get("description_domain").is_some());
        assert!(bco.get("execution_domain").is_some());

        // provenance_domain carries archetype id + rationale
        let prov = &bco["provenance_domain"];
        assert_eq!(prov["matched_archetype"], "bulk_rnaseq_de");
        assert!(prov["rationale"].as_str().unwrap().contains("bulk RNA-seq"));

        // pipeline_steps has one entry per composed atom
        let steps = &bco["description_domain"]["pipeline_steps"];
        assert_eq!(steps.as_array().unwrap().len(), 1);
        assert_eq!(steps[0]["name"], "differential_expression");
        assert_eq!(steps[0]["edam_data"], "data:0951");
    }

    #[test]
    fn emit_bco_writes_file_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let composition = sample_composition();
        let intake = sample_intake();
        emit_bco(&composition, &intake, tmp.path()).expect("emit_bco");
        let bco_path = tmp.path().join("bco.json");
        assert!(bco_path.exists(), "bco.json must be written");

        // Round-trip: the on-disk bytes must equal what `build_bco`
        // produces for the same inputs — `build_bco` is now fully
        // deterministic (no wall-clock read), so emit and rebuild
        // agree byte-for-byte.
        let raw = std::fs::read(&bco_path).unwrap();
        let rebuilt =
            serde_json::to_vec_pretty(&build_bco(&composition, &intake)).unwrap();
        assert_eq!(raw, rebuilt, "emitted bco.json must equal a fresh build_bco");
        let parsed: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(
            parsed["spec_version"],
            json!("https://w3id.org/ieee/ieee-2791-schema/2791object.json")
        );
        assert!(parsed.get("io_domain").is_some());
        assert!(parsed.get("parametric_domain").is_some());
        assert!(parsed.get("error_domain").is_some());
    }

    #[test]
    fn build_bco_at_is_byte_deterministic_with_fixed_time() {
        // The byte-deterministic contract holds when `created_at` is
        // pinned. Two builds with the same inputs must produce
        // byte-identical JSON.
        let composition = sample_composition();
        let intake = sample_intake();
        let fixed = "2026-05-15T00:00:00+00:00";
        let a = serde_json::to_vec_pretty(&build_bco_at(&composition, &intake, fixed)).unwrap();
        let b = serde_json::to_vec_pretty(&build_bco_at(&composition, &intake, fixed)).unwrap();
        assert_eq!(a, b, "two BCO builds diverged");
    }

    #[test]
    fn build_bco_is_byte_deterministic_across_calls() {
        // det-02 — the PRODUCTION entry point `build_bco` must be
        // byte-reproducible: no wall-clock read leaks into
        // `created`/`modified`, and the content-addressed `etag` is
        // therefore stable too. Two calls with identical inputs must
        // hash identically and serialize identically.
        let composition = sample_composition();
        let intake = sample_intake();
        let a = build_bco(&composition, &intake);
        let b = build_bco(&composition, &intake);
        assert_eq!(
            a["etag"], b["etag"],
            "build_bco etag must be deterministic across calls"
        );
        assert_eq!(
            a["provenance_domain"]["created"], b["provenance_domain"]["created"],
            "created timestamp must be deterministic"
        );
        let a_bytes = serde_json::to_vec_pretty(&a).unwrap();
        let b_bytes = serde_json::to_vec_pretty(&b).unwrap();
        assert_eq!(a_bytes, b_bytes, "two build_bco outputs diverged byte-wise");
        // The created/modified timestamp must be a valid RFC-3339 value
        // in the deterministic 2026..2076 range (never a wall-clock
        // value from the host).
        let created = a["provenance_domain"]["created"].as_str().unwrap();
        let parsed = chrono::DateTime::parse_from_rfc3339(created).unwrap();
        let lower: chrono::DateTime<chrono::Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let upper: chrono::DateTime<chrono::Utc> = "2076-01-01T00:00:00Z".parse().unwrap();
        assert!(
            parsed.with_timezone(&chrono::Utc) >= lower
                && parsed.with_timezone(&chrono::Utc) < upper,
            "deterministic emit-time {created} out of documented range"
        );
    }

    #[test]
    fn build_bco_with_clock_threads_frozen_clock() {
        // det-02 — `build_bco_with_clock` lets the emit pipeline reuse
        // its `FrozenClock` so the BCO timestamp aligns with the
        // RO-Crate's. Two builds with the same FrozenClock are
        // byte-identical and reflect the clock's instant.
        use crate::clock::FrozenClock;
        let composition = sample_composition();
        let intake = sample_intake();
        let pinned: chrono::DateTime<chrono::Utc> = "2026-03-04T05:06:07Z".parse().unwrap();
        let clock = FrozenClock { at: pinned };
        let a = build_bco_with_clock(&composition, &intake, &clock);
        let b = build_bco_with_clock(&composition, &intake, &clock);
        assert_eq!(serde_json::to_vec(&a).unwrap(), serde_json::to_vec(&b).unwrap());
        assert_eq!(
            a["provenance_domain"]["created"].as_str().unwrap(),
            pinned.to_rfc3339()
        );
    }

    #[test]
    fn emit_bco_writes_valid_rfc3339_timestamps() {
        // The live emit path must populate `created` and `modified` with
        // parseable RFC-3339 strings. Production calls `build_bco`, which
        // derives a deterministic RFC-3339 emit-time from content.
        let tmp = tempfile::tempdir().unwrap();
        let composition = sample_composition();
        let intake = sample_intake();
        emit_bco(&composition, &intake, tmp.path()).unwrap();
        let raw = std::fs::read_to_string(tmp.path().join("bco.json")).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        let created = parsed["provenance_domain"]["created"].as_str().unwrap();
        let modified = parsed["provenance_domain"]["modified"].as_str().unwrap();
        assert!(
            chrono::DateTime::parse_from_rfc3339(created).is_ok(),
            "created is not RFC-3339: {created}"
        );
        assert!(
            chrono::DateTime::parse_from_rfc3339(modified).is_ok(),
            "modified is not RFC-3339: {modified}"
        );
    }

    #[test]
    fn etag_changes_when_composition_changes() {
        // R4-Sci-4 — pin the timestamp via `build_bco_at` so the
        // etag-stability test isolates composition changes from
        // wall-clock drift in `created`/`modified`.
        let intake = sample_intake();
        let mut a = sample_composition();
        let mut b = sample_composition();
        b.rationale = "Different rationale".into();
        let fixed = "2026-05-15T00:00:00+00:00";
        let bco_a = build_bco_at(&a, &intake, fixed);
        let bco_b = build_bco_at(&b, &intake, fixed);
        assert_ne!(
            bco_a["etag"], bco_b["etag"],
            "etag must reflect content changes"
        );

        // Conversely: trivial nondestructive read mutation doesn't
        // affect the hash because we only read fields once.
        a.match_score = 99; // not surfaced in BCO
        let bco_a2 = build_bco_at(&a, &intake, fixed);
        assert_eq!(
            bco_a2["etag"], bco_a["etag"],
            "etag should ignore non-emitted fields"
        );
    }

    /// R4-Sci-4 — the etag must be present on the emitted doc, but
    /// the input to the hash must NOT include the `etag` property.
    /// Verify the etag is absent from the canonical bytes by
    /// re-computing it ourselves with `etag` removed and asserting
    /// equality.
    #[test]
    fn etag_excludes_etag_field_from_hash_input() {
        let composition = sample_composition();
        let intake = sample_intake();
        let fixed = "2026-05-15T00:00:00+00:00";
        let bco = build_bco_at(&composition, &intake, fixed);
        let recorded_etag = bco["etag"].as_str().unwrap().to_string();

        // Recompute: clone the doc, strip the etag field, serialize
        // canonically, hash.
        let mut stripped = bco.clone();
        stripped.as_object_mut().unwrap().remove("etag");
        let bytes = serde_json::to_vec(&stripped).unwrap();
        let recomputed = crate::hash_utils::sha256_hex(&bytes);
        assert_eq!(
            recorded_etag, recomputed,
            "etag must be hash of the doc with the etag field absent"
        );
    }

    /// R4-Sci-4 — IO/parametric/error domains are present and have
    /// the expected IEEE 2791-2020 shape.
    #[test]
    fn build_bco_populates_io_parametric_error_domains() {
        // Augment the sample composition's atom with an attribute and
        // a declared input/output port so the new domains have data
        // to flatten.
        use crate::workflow_contracts::port::{Cardinality, PortContract, PortPrivacyClass};
        use crate::workflow_contracts::semantic_type::SemanticType;
        let mut composition = sample_composition();
        let composed = &mut composition.atoms[0];
        composed
            .atom
            .attributes
            .insert("padj_threshold".into(), serde_json::json!(0.05));
        let stype = SemanticType::OntologyTerm {
            iri: "data:0951".into(),
            label: "Statistical estimate score".into(),
            ontology_version: None,
        };
        composed.atom.inputs.push(PortContract {
            name: "counts".into(),
            semantic_type: stype.clone(),
            physical_format: None,
            structural_schema: None,
            ontology_terms: Vec::new(),
            modality: None,
            organism: None,
            genome_build: None,
            annotation_version: None,
            coordinate_system: None,
            units: None,
            normalization_state: None,
            statistical_state: None,
            privacy_class: PortPrivacyClass::default(),
            cardinality: Cardinality::default(),
            validators: Vec::new(),
            constraints: Vec::new(),
            facets: std::collections::BTreeMap::new(),
        });
        composed.atom.outputs.push(PortContract {
            name: "deg_table".into(),
            semantic_type: stype,
            physical_format: None,
            structural_schema: None,
            ontology_terms: Vec::new(),
            modality: None,
            organism: None,
            genome_build: None,
            annotation_version: None,
            coordinate_system: None,
            units: None,
            normalization_state: None,
            statistical_state: None,
            privacy_class: PortPrivacyClass::default(),
            cardinality: Cardinality::default(),
            validators: Vec::new(),
            constraints: Vec::new(),
            facets: std::collections::BTreeMap::new(),
        });
        let intake = sample_intake();
        let bco = build_bco_at(&composition, &intake, "2026-05-15T00:00:00+00:00");
        let io = &bco["io_domain"];
        assert!(io.is_object(), "io_domain must be an object");
        assert_eq!(io["input_subdomain"][0]["step"], "differential_expression");
        assert_eq!(io["input_subdomain"][0]["uri"]["filename"], "counts");
        assert_eq!(io["output_subdomain"][0]["uri"]["filename"], "deg_table");
        let pd = bco["parametric_domain"].as_array().unwrap();
        assert!(pd
            .iter()
            .any(|e| e["param"] == "padj_threshold" && e["value"] == serde_json::json!(0.05)));
        let ed = &bco["error_domain"];
        assert!(ed["empirical_error"].is_array());
        assert!(ed["algorithmic_error"].is_array());
    }

    #[test]
    fn backward_chain_composition_uses_placeholder_archetype_id() {
        // When matched_archetype is None (backward-chain path),
        // object_id falls back to "backward-chain" so the document
        // stays well-formed.
        let mut composition = sample_composition();
        composition.matched_archetype = None;
        let intake = sample_intake();
        let bco = build_bco(&composition, &intake);
        assert_eq!(
            bco["provenance_domain"]["matched_archetype"],
            "backward-chain"
        );
        assert!(bco["object_id"]
            .as_str()
            .unwrap()
            .contains("backward-chain"));
    }

    #[test]
    fn pipeline_steps_preserve_atom_order() {
        // BCO pipeline_steps must emit in composition order so
        // SMEs reading the document see deps before consumers.
        let mut composition = sample_composition();
        let prep = AtomDefinition {
            id: "qc_preprocessing".into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            confirmatory: false,
            discovery_kind: None,
            description: "QC step.".into(),
            edam_operation: "ecaax:scrnaseq_cell_qc".into(),
            edam_data: Some("data:3917".into()),
            edam_format: Some("format:3590".into()),
            assignee: AtomAssignee::Agent,
            depends_on: vec![],
            excludes: vec![],
            attributes: BTreeMap::new(),
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: None,
            resource_profile: None,
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            required_artifacts: vec![],
            validators: vec![],
            runtime_packages: Default::default(),
            parameters: Vec::new(),
            provenance: None,
            estimated_duration: None,
            safety: Default::default(),
        };
        composition.atoms.insert(
            0,
            ComposedAtom {
                stage_id: "qc_preprocessing".into(),
                atom: prep,
                depends_on: vec![],
                required: true,
                bindings: Vec::new(),
                container: None,
            },
        );
        let intake = sample_intake();
        let bco = build_bco(&composition, &intake);
        let steps = bco["description_domain"]["pipeline_steps"]
            .as_array()
            .unwrap();
        assert_eq!(steps[0]["name"], "qc_preprocessing");
        assert_eq!(steps[1]["name"], "differential_expression");
        assert_eq!(steps[0]["step_number"], 0);
        assert_eq!(steps[1]["step_number"], 1);
    }
}
