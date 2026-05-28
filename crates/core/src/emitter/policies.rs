//! Policy emission helpers that write derived JSON into
//! `package/policies/`.
//!
//! Every policy the emitter ships goes through `write_policy` so new
//! policies extend a single list rather than growing `emit_package`
//! with per-policy branches. The compute-profile + GPU-capability
//! emitters additionally validate their source YAML against a sidecar
//! schema before writing.
//!
//! The runtime-prereqs + per-atom-prereqs emitters are co-located
//! here because they share the same target directory
//! (`package_dir/policies/`) and the container spec +
//! memory-discipline policies sit next to them.

use crate::intake_facts::IntakeFacts;
use anyhow::{anyhow, Context, Result};
use std::path::Path;

/// Write a serializable `value` as pretty-printed JSON to
/// `package_dir/policies/<name>.json`. Every derived policy the
/// emitter ships goes through this helper so new policies extend a
/// single list rather than growing `emit_package` with per-policy
/// branches.
pub(super) fn write_policy<T: serde::Serialize>(
    package_dir: &Path,
    name: &str,
    value: &T,
) -> Result<()> {
    let policies_dir = package_dir.join("policies");
    let payload = serde_json::to_string_pretty(value)
        .with_context(|| format!("serializing policy '{}'", name))?;
    let target = policies_dir.join(format!("{}.json", name));
    // Atomic write (.tmp + fsync + rename + parent fsync) so a crash
    // mid-emit never leaves a half-written policy on disk that the
    // harness pre-flight could pick up. `atomic_write_bytes_sync`
    // creates the parent `policies/` dir lazily.
    crate::fs_helpers::atomic_write_bytes_sync(&target, payload.as_bytes())
        .with_context(|| format!("writing {}.json", name))?;
    Ok(())
}

/// Serialize the compute-profiles YAML into `policies/compute-resource-policy.json`.
/// Soft-skips when the profiles.yaml isn't present (e.g., running from a
/// tree without `config/compute-profiles/`) so existing `make ivd`
/// callers that don't pass `compute_profiles_dir` stay byte-identical.
/// Phase-1 additions (`tool_thread_curves`, `env_overrides_template`,
/// `phase_thread_counts`) flow through unchanged because this helper
/// converts the YAML as-is — no struct projection in the emit path.
///
/// When the `compute-profiles.schema.json` sidecar
/// exists, validate the YAML against it before emitting. Schema-
/// drift surfaces at emit time rather than as a runtime resolver
/// misroute. Soft-skip when the sidecar is absent so legacy
/// installations without the schema continue to emit.
pub(super) fn emit_compute_profile_policy(
    package_dir: &Path,
    profiles_dir: Option<&Path>,
) -> Result<()> {
    let Some(profiles_dir) = profiles_dir else {
        return Ok(());
    };
    let yaml_path = profiles_dir.join("profiles.yaml");
    if !yaml_path.exists() {
        return Ok(());
    }
    let as_json = crate::fs_helpers::read_yaml_as_json(&yaml_path)?;

    // Schema-validate when sidecar present. Soft-skip
    // when missing so emitter byte-identity holds for trees without
    // the schema.
    let schema_path = profiles_dir.join("compute-profiles.schema.json");
    if schema_path.exists() {
        let compiled = crate::schema_helpers::compile_schema_from_path_cached(&schema_path)?;
        let messages: Vec<String> = match compiled.validate(&as_json) {
            Ok(()) => Vec::new(),
            Err(errs) => errs.map(|e| format!("{e}")).collect(),
        };
        if !messages.is_empty() {
            anyhow::bail!(
                "compute-profiles.yaml fails schema validation: {}",
                messages.join("; ")
            );
        }
    }

    write_policy(package_dir, "compute-resource-policy", &as_json)
}

/// Serialize `gpu-capability-map.yaml` into
/// `policies/gpu-capability-policy.json`. The agent reads this at
/// runtime to route a chosen method to its GPU impl when hardware +
/// AMI support it; `prompt_role.txt`'s "Hardware-aware execution"
/// rule 5 probes `which <binary>` before invoking the GPU path and
/// falls back to the CPU impl on missing binary.
///
/// Soft-skips when the YAML file is absent so existing callers that
/// don't ship a gpu-capability-map stay byte-identical. When the file
/// exists AND its `.schema.json` sidecar exists, the emitter validates
/// before writing so shape drift surfaces at emit time rather than as
/// a runtime agent misrouting.
pub(super) fn emit_gpu_capability_policy(
    package_dir: &Path,
    profiles_dir: Option<&Path>,
) -> Result<()> {
    let Some(profiles_dir) = profiles_dir else {
        return Ok(());
    };
    let yaml_path = profiles_dir.join("gpu-capability-map.yaml");
    if !yaml_path.exists() {
        return Ok(());
    }
    let as_json = crate::fs_helpers::read_yaml_as_json(&yaml_path)?;

    // Validate against the schema sidecar when present. Missing
    // sidecar is a soft error (transitional-rollout contract) — same
    // pattern as policy_schema::load_and_validate.
    let schema_path = profiles_dir.join("gpu-capability-map.schema.json");
    if schema_path.exists() {
        validate_gpu_capability_against_schema(&schema_path, &yaml_path, &as_json)?;
    }

    write_policy(package_dir, "gpu-capability-policy", &as_json)
}

/// Helper for `emit_gpu_capability_policy` that keeps the compiled
/// schema borrow within a tight scope — avoids the lifetime snag
/// where `compiled.validate()`'s error iterator borrows compiled past
/// the end of the outer `if` block under 2021-edition NLL.
fn validate_gpu_capability_against_schema(
    schema_path: &Path,
    yaml_path: &Path,
    as_json: &serde_json::Value,
) -> Result<()> {
    let compiled = crate::schema_helpers::compile_schema_from_path_cached(schema_path)?;
    let messages: Vec<String> = match compiled.validate(as_json) {
        Ok(()) => return Ok(()),
        Err(errs) => errs
            .map(|e| format!("  at {}: {}", e.instance_path, e))
            .collect(),
    };
    Err(anyhow::anyhow!(
        "gpu-capability-map {} failed schema validation ({} violation(s)):\n{}",
        yaml_path.display(),
        messages.len(),
        messages.join("\n")
    ))
}

/// Serialize IntakeFacts into `policies/intake-facts.json`.
pub(super) fn emit_intake_facts(package_dir: &Path, facts: &IntakeFacts) -> Result<()> {
    write_policy(package_dir, "intake-facts", facts)
}

/// Write `policies/runtime-prereqs.json`. When `manifest` is
/// `None`, writes a fresh empty-but-valid v1 manifest so
/// downstream consumers (BagIt walk, harness pre-flight,
/// verify-reproducibility) always find the file. Empty manifests are
/// understood by the pre-flight as "no derived image — fall through
/// to host or per-task pin."
pub(super) fn emit_runtime_prereqs(
    package_dir: &Path,
    manifest: Option<&crate::runtime_prereqs::RuntimePrereqs>,
) -> Result<()> {
    let owned;
    let payload = match manifest {
        Some(m) => m,
        None => {
            owned = crate::runtime_prereqs::RuntimePrereqs::new();
            &owned
        }
    };
    write_policy(package_dir, "runtime-prereqs", payload)?;

    // When the manifest is buildable, render the deterministic
    // Dockerfile alongside the manifest. Empty / non-buildable
    // manifests skip this — keeps legacy packages byte-identical on
    // disk.
    if let Some(dockerfile) = crate::derived_image::render_dockerfile(payload) {
        let dockerfile_path = package_dir.join("runtime/derived-image.Dockerfile");
        crate::fs_helpers::atomic_write_bytes_sync(&dockerfile_path, dockerfile.as_bytes())
            .context("writing runtime/derived-image.Dockerfile")?;
        // Copy the install-proxy shims into the package
        // so the Dockerfile's `COPY runtime/install-proxy/...` lines
        // resolve against the build context. Gated on the same
        // is-buildable condition as the Dockerfile itself; non-bio
        // packages without a manifest stay byte-identical on disk.
        super::copy_libs::copy_install_proxy(package_dir)
            .context("copying install-proxy shims into runtime/install-proxy/")?;
    }

    Ok(())
}

/// Write `policies/atom-prereqs/<atom_id>.json` per atom whose
/// `runtime_packages` is buildable. The harness reads these at task
/// launch time when `SWFC_PER_TASK_IMAGES=1`, derives a per-atom
/// content hash, and builds (or cache-hits) one image per unique
/// atom hash. Atoms with empty / unbuildable manifests get no file
/// — the harness then falls back to `atom.preferred_container.image`
/// for that task.
///
/// Determinism: serialized via `serde_json::to_string_pretty` (sorted
/// `BTreeSet` collections + `BTreeMap` key ordering); two emits with
/// the same input map produce byte-identical files. Each file is
/// written via `tmp -> rename` so a partial write never lands on
/// disk. The wrapping directory is created lazily so a fully-empty
/// (all-unbuildable) input map leaves no `policies/atom-prereqs/`
/// dir behind — keeps the package surface minimal.
pub(super) fn emit_per_atom_runtime_prereqs(
    package_dir: &Path,
    map: &std::collections::BTreeMap<String, crate::runtime_prereqs::RuntimePrereqs>,
    union_runtime_prereqs: Option<&crate::runtime_prereqs::RuntimePrereqs>,
) -> Result<()> {
    // Atom-level manifests don't set `base_image` — that comes from the
    // archetype's `runtime_baseline`. Inject the union manifest's
    // base_image into each atom's clone before evaluating `is_buildable`
    // / serializing, so atoms with apt/dnf deltas under a properly
    // configured archetype emit a per-atom file even when they
    // themselves didn't declare a base. Without this the atom-prereqs/
    // directory stayed empty even after archetype + atom YAMLs were
    // populated, because `is_buildable()` short-circuits on
    // `base_image.is_none()`.
    let union_base = union_runtime_prereqs.and_then(|r| r.base_image.clone());
    let materialised: std::collections::BTreeMap<String, crate::runtime_prereqs::RuntimePrereqs> =
        map.iter()
            .map(|(id, m)| {
                let mut clone = m.clone();
                if clone.base_image.is_none() {
                    clone.base_image = union_base.clone();
                }
                (id.clone(), clone)
            })
            .collect();
    let buildable: Vec<(&String, &crate::runtime_prereqs::RuntimePrereqs)> = materialised
        .iter()
        .filter(|(_, m)| m.is_buildable())
        .collect();
    if buildable.is_empty() {
        return Ok(());
    }
    let dir = package_dir.join("policies").join("atom-prereqs");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating per-atom prereqs dir at {}", dir.display()))?;
    for (atom_id, manifest) in buildable {
        // 04 shape: atom_id flows into a file path
        // segment so it must not contain `/`, `..`, or NUL. Atom ids
        // already pass `crate::atom::validate_atom_id` at registry
        // load, but defense-in-depth here too — a malformed key in
        // the caller's map must refuse to land on disk rather than
        // escape the package directory.
        if atom_id.is_empty()
            || atom_id.contains('/')
            || atom_id.contains('\0')
            || atom_id.split('/').any(|s| s == "..")
        {
            return Err(anyhow!(
                "per-atom prereqs map carries invalid atom_id {:?}",
                atom_id
            ));
        }
        let path = dir.join(format!("{atom_id}.json"));
        let raw = serde_json::to_string_pretty(manifest)
            .with_context(|| format!("serializing per-atom prereqs for {atom_id}"))?;
        // Atomic write (.tmp + fsync + rename) so a crash mid-emit
        // never leaves a half-written manifest the pre-flight could
        // pick up.
        crate::fs_helpers::atomic_write_bytes_sync(&path, raw.as_bytes())
            .with_context(|| format!("atomic write per-atom prereqs for {atom_id}"))?;
    }
    Ok(())
}

/// Serialize the taxonomy's `preferred_container` into
/// `policies/container.json` so the agent scripts can read it without
/// parsing YAML. `{ "image": "<tag>" | null }`. Always emitted — a null
/// image means "use host environment".
pub(super) fn emit_container_spec(
    package_dir: &Path,
    preferred_container: Option<&str>,
) -> Result<()> {
    #[derive(serde::Serialize)]
    struct ContainerSpec<'a> {
        image: Option<&'a str>,
    }
    write_policy(
        package_dir,
        "container",
        &ContainerSpec {
            image: preferred_container,
        },
    )
}

/// Memory-discipline policy the agent consults before materializing a
/// large dense matrix. Thresholds are deliberately conservative —
/// crossing them is the trigger to reach for on-disk libraries
/// (BPCells in R, anndata-backed / zarr in Python), not a hard
/// refusal. Static for now; future work can make thresholds
/// compute-profile-aware.
pub(super) fn emit_memory_discipline_policy(package_dir: &Path) -> Result<()> {
    // The values below describe the contract the agent follows, not
    // a dynamic budget. Raising them requires updating the downstream
    // agent prompts + BPCells provisioning; see provision_r_bioconductor.sh.
    let payload = serde_json::json!({
        "schema_version": 1,
        "max_dense_matrix_gb": 20,
        "large_cohort_cell_threshold_k": 100,
        "on_disk_library_hints": {
            "R": ["BPCells", "DelayedArray", "HDF5Array"],
            "python": ["anndata (backed='r')", "zarr", "h5py"]
        },
        "guidance": [
            "Never materialize a dense cell×gene matrix larger than max_dense_matrix_gb; switch to an on_disk_library_hints entry instead.",
            "For cohorts above large_cohort_cell_threshold_k total cells, prefer per-compartment or per-batch merges over a single global merge.",
            "Seurat v5: use BPCells::write_matrix_dir + open_matrix_dir as the assay backing for SCTransform v2 on large cohorts.",
            "anndata: pass backed='r' when reading and use concat(...) with on-disk writes instead of loading every file into RAM.",
            "If an upstream stage produced per-compartment artifacts and a later stage requests a global merge, verify the global merge is actually consumed downstream before materializing it — a redundant merge has caused production OOMs in the past."
        ],
        "references": [
            "https://bnprks.github.io/BPCells/",
            "https://anndata.readthedocs.io/en/stable/generated/anndata.AnnData.html#anndata.AnnData",
            "https://satijalab.org/seurat/articles/seurat5_bpcells_interaction_vignette"
        ]
    });
    write_policy(package_dir, "memory-discipline", &payload)
}
