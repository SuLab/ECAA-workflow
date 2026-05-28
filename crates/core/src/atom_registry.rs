//! Atom-registry loader.
//!
//! Walks a directory of `<id>.yaml` atom files, validates each against
//! `_atom.schema.json` (embedded via `include_str!`), deserializes into
//! [`AtomDefinition`], and yields a sorted-by-id collection. Plus
//! `find_producers` for the composer's backward-chaining lookup
//! (S7.2) and `validate_consistency` for the load-time invariant
//! checks per plan §3.3.
//!
//! # Discipline
//!
//! Schema-validate-before-deserialize: serde will happily load YAML
//! that's syntactically valid but missing required fields or carrying
//! unknown keys. The schema catches both before we materialize a Rust
//! struct that would then need `Result<>` unwrapping at every call
//! site.
//!
//! BTreeMap keying preserves byte-deterministic iteration order, so
//! the composer's output is reproducible across runs.

use crate::atom::AtomDefinition;
use anyhow::{anyhow, Context, Result};
use jsonschema::JSONSchema;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// Embedded schema. The compile-time include guarantees the schema
/// ships with the binary; no runtime path-resolution required.
const ATOM_SCHEMA_JSON: &str = include_str!("../../../config/stage-atoms/_atom.schema.json");

/// In-memory atom catalog. Keyed by `id`; `BTreeMap` so iteration is
/// deterministic.
#[derive(Debug, Clone, Default)]
pub struct AtomRegistry {
    atoms: BTreeMap<String, AtomDefinition>,
}

impl AtomRegistry {
    /// Walk `dir`, load every `*.yaml` file (excluding `_*.yaml` —
    /// those are schema sidecars or shared fragments). Returns an
    /// error on the first malformed atom.
    ///
    /// `dir` is typically `config/stage-atoms/`. Subdirectories are
    /// not recursed (per ADR / plan §3.1 row "Flat
    /// `config/stage-atoms/*.yaml`").
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let schema = Self::compiled_schema()?;
        let mut atoms = BTreeMap::new();
        if !dir.exists() {
            // An empty dir is allowed (the composer fallback
            // logic detects empty registries and routes through the
            // legacy builder).
            return Ok(Self { atoms });
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("reading atom directory {}", dir.display()))?
            .filter_map(|r| r.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension().and_then(|s| s.to_str()) == Some("yaml")
                    && !p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with('_'))
                        .unwrap_or(false)
            })
            .collect();
        entries.sort();
        for path in entries {
            let raw = crate::fs_helpers::read_to_string_ctx(&path)?;
            // Parse to serde_json::Value for jsonschema validation;
            // jsonschema works on serde_json::Value, but the YAML
            // syntax is what humans author. serde_yml has a
            // documented round-trip via serde_json::to_value.
            let yaml_val: serde_yml::Value = serde_yml::from_str(&raw)
                .with_context(|| format!("parsing atom YAML {}", path.display()))?;
            let parsed: Value = serde_json::to_value(&yaml_val)
                .with_context(|| format!("yaml→json reshape for {}", path.display()))?;
            // Schema validate first; serde_yml::from_str on a
            // missing-field shape would panic at deserialize time
            // with a message that doesn't point at the source line.
            if let Err(errors) = schema.validate(&parsed) {
                let msgs: Vec<String> = errors
                    .map(|e| format!("{} at {}", e, e.instance_path))
                    .collect();
                return Err(anyhow!(
                    "atom {} failed schema validation:\n  - {}",
                    path.display(),
                    msgs.join("\n  - ")
                ));
            }
            let atom: AtomDefinition = serde_json::from_value(parsed)
                .with_context(|| format!("deserializing atom {}", path.display()))?;
            // Filename stem must match the atom id for byte-
            // deterministic registry iteration; otherwise a future
            // atom rename could leave a stale file name picked up by
            // the loader.
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("atom path {} has no stem", path.display()))?;
            if stem != atom.id {
                return Err(anyhow!(
                    "atom file {} has stem {} but declares id {}; rename one to match",
                    path.display(),
                    stem,
                    atom.id
                ));
            }
            if atoms.insert(atom.id.clone(), atom.clone()).is_some() {
                return Err(anyhow!(
                    "duplicate atom id {} (second file: {})",
                    atom.id,
                    path.display()
                ));
            }
        }
        Ok(Self { atoms })
    }

    /// Iterate every atom in id-sorted order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &AtomDefinition)> {
        self.atoms.iter()
    }

    /// Look up an atom by id.
    pub fn get(&self, id: &str) -> Option<&AtomDefinition> {
        self.atoms.get(id)
    }

    /// Number of atoms loaded.
    pub fn len(&self) -> usize {
        self.atoms.len()
    }

    /// Is empty.
    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    /// Return every bare discover axis that `composer_v4::synthesize_discover_companions`
    /// can emit a `discover_<axis>` companion for, sorted for byte-stable output.
    ///
    /// This is the authoritative source for the server's auto-approve
    /// allowlist (`crates/server/src/chat_routes/tasks/blocker.rs::auto_approve_discoveries`)
    /// and for the e2e helpers' default in `e2e/helpers/atomsLifecycle.ts`.
    /// Mirrors `derive_axis` exactly so adding a `candidate_tools` block
    /// or `method_choice.deferred_to` to a new atom automatically extends
    /// the allowlist with no other code change required.
    pub fn discover_axes(&self) -> std::collections::BTreeSet<String> {
        self.atoms
            .values()
            .filter_map(crate::composer_v4::discover_companion_synthesis::derive_axis)
            .collect()
    }

    /// Return every atom whose `edam_data` matches `target_data` and
    /// (optionally) whose `edam_format` matches `target_format`. The
    /// composer's backward-chain (S7.2) feeds this with the goal's
    /// data type to find candidate producer atoms.
    ///
    /// Order preserved (BTreeMap iteration) so the composer's output
    /// is deterministic when multiple producers tie on the EDAM
    /// criteria — first-match-wins resolves to the first producer
    /// alphabetically by id.
    pub fn find_producers<'a>(
        &'a self,
        target_data: &'a str,
        target_format: Option<&'a str>,
    ) -> impl Iterator<Item = &'a AtomDefinition> + 'a {
        self.atoms.values().filter(move |a| {
            let data_match = a
                .edam_data
                .as_deref()
                .map(|d| d == target_data)
                .unwrap_or(false);
            let format_match = match (target_format, a.edam_format.as_deref()) {
                (None, _) => true,
                (Some(want), Some(got)) => want == got,
                (Some(_), None) => false,
            };
            data_match && format_match
        })
    }

    /// Walk every atom and assert load-time invariants (plan §3.3).
    ///
    /// Load-time invariants (plan §3.3):
    /// - `depends_on` ids reference atoms that exist in the registry
    /// - `excludes` ids reference atoms that exist in the registry,
    ///   or parse as CEL expressions when prefixed with `cel:` — the
    ///   prefix is validated at load time so a malformed CEL fails the
    ///   registry build, not a downstream composer call. The compiled
    ///   `Program` is intentionally discarded here (we revalidate at
    ///   composer eval time so a stale program doesn't hold a context
    ///   that drifted with the atom YAML); the load-time check is
    ///   structural only.
    /// - `method_choice.deferred_to` references a discovery atom
    /// - duplicate-id-different-version is impossible by construction
    ///   (BTreeMap insert), but we re-check the version field is a
    ///   parseable semver string for forward compatibility.
    /// - role==discovery atoms have `discovery_kind: Some(_)` (the
    ///   schema enforces this too, but a defensive runtime check
    ///   catches schema-bypass via direct deserialization in tests).
    ///
    /// The v4 composer adds six-item formal validation: acyclicity,
    /// goal reachability, input satisfiability, exclusion consistency,
    /// attribute resolution, gate well-formedness.
    pub fn validate_consistency(&self) -> Result<()> {
        for (id, atom) in &self.atoms {
            for dep in &atom.depends_on {
                if dep == id {
                    // Self-loop is structurally a one-atom cycle the
                    // composer's would_create_cycle check then rejects
                    // every consumer of. Refuse at load time so the
                    // failure message is sharp.
                    return Err(anyhow!("atom {} depends_on itself (self-loop)", id));
                }
                if !self.atoms.contains_key(dep) {
                    return Err(anyhow!("atom {} depends_on unknown atom {}", id, dep));
                }
            }
            for excl in &atom.excludes {
                // `cel:` prefix opts the entry out of
                // atom-id resolution and into expression compilation.
                // Anything else stays an atom-id reference for the
                // legacy literal-list semantics.
                if let Some(cel_src) = excl.strip_prefix("cel:") {
                    let cel_src = cel_src.trim();
                    if cel_src.is_empty() {
                        return Err(anyhow!(
                            "atom {} excludes entry `cel:` must carry a non-empty CEL \
                             expression after the prefix",
                            id
                        ));
                    }
                    cel_interpreter::Program::compile(cel_src).map_err(|e| {
                        anyhow!(
                            "atom {} excludes CEL expression `{}` failed to compile: {:?}",
                            id,
                            cel_src,
                            e
                        )
                    })?;
                    continue;
                }
                if !self.atoms.contains_key(excl) {
                    return Err(anyhow!(
                        "atom {} excludes unknown atom {} (or, if intended as CEL, \
                         prefix the entry with `cel:` per S7.3)",
                        id,
                        excl
                    ));
                }
            }
            if let Some(mc) = &atom.method_choice {
                let target = self.atoms.get(&mc.deferred_to);
                match target {
                    None => {
                        return Err(anyhow!(
                            "atom {} method_choice.deferred_to {} doesn't exist",
                            id,
                            mc.deferred_to
                        ));
                    }
                    Some(t) if t.role != crate::atom::AtomRole::Discovery => {
                        return Err(anyhow!(
                            "atom {} method_choice.deferred_to {} is not a discovery atom",
                            id,
                            mc.deferred_to
                        ));
                    }
                    Some(_) => {}
                }
            }
            if matches!(atom.role, crate::atom::AtomRole::Discovery)
                && atom.discovery_kind.is_none()
            {
                return Err(anyhow!(
                    "atom {} role==discovery requires discovery_kind",
                    id
                ));
            }
            // IterateSpec gate — the YAML schema rejects
            // `max_iterations: 0`, but a Rust-side constructor or a
            // future programmatic registry build path can still produce
            // an atom with a zero ceiling. A zero ceiling means the
            // runtime expansion would emit a zero-task scaffold and the
            // agent would never converge, so the failure must surface
            // here rather than at compose time.
            if let Some(iter) = &atom.iterate {
                if iter.max_iterations == 0 {
                    return Err(anyhow!(
                        "atom {} iterate.max_iterations must be > 0 \
                         (zero ceiling produces an empty iterate scaffold)",
                        id
                    ));
                }
                if iter.min_iterations > iter.max_iterations {
                    return Err(anyhow!(
                        "atom {} iterate.min_iterations ({}) must be <= max_iterations ({})",
                        id,
                        iter.min_iterations,
                        iter.max_iterations
                    ));
                }
            }
            // Cheap semver shape check — full semver parsing lives
            // in the upcoming crates/core::edam.rs (S4.8) so we
            // don't pull semver as a dep here; M.M.M is the floor.
            // Each of the three components must also be an ASCII-digit
            // run (semver allows pre-release / build suffixes after a
            // `-` or `+` on the patch field — strip those before the
            // numeric check) so values like `1.x.0` or `1..0` are
            // refused at load time instead of bleeding into the atom
            // catalog where they break version-aware caches.
            let parts: Vec<&str> = atom.version.split('.').collect();
            if parts.len() < 3 || parts.iter().any(|p| p.is_empty()) {
                return Err(anyhow!(
                    "atom {} has unparseable version {}",
                    id,
                    atom.version
                ));
            }
            for (idx, part) in parts.iter().enumerate().take(3) {
                // The patch field (idx==2) may carry a `-pre`/`+build`
                // semver suffix per the spec; strip that before the
                // digit check.
                let core = if idx == 2 {
                    part.split(['-', '+']).next().unwrap_or(part)
                } else {
                    part
                };
                if core.is_empty() || !core.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(anyhow!(
                        "atom {} version `{}` field `{}` is not a non-empty ASCII-digit run",
                        id,
                        atom.version,
                        part
                    ));
                }
            }
        }
        // Per-atom safety consistency. Runs AFTER the
        // structural checks above (so e.g. dangling-id failures fire
        // first) and surfaces EVERY violation across EVERY atom in a
        // single aggregated error — `validate_atom_safety` returns a
        // Vec so the registry can refuse to load with the full set in
        // one message, rather than forcing an author to re-run the
        // lint after each fix.
        let mut safety_errors: Vec<crate::atom_safety::SafetyConsistencyError> = Vec::new();
        for atom in self.atoms.values() {
            safety_errors.extend(crate::atom_safety::validate_atom_safety(atom));
        }
        if !safety_errors.is_empty() {
            return Err(anyhow!(
                "atom registry safety lint failed:\n  - {}",
                safety_errors
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("\n  - ")
            ));
        }
        Ok(())
    }

    /// Build a new [`AtomRegistry`] that contains every atom from
    /// `self` plus the supplied overlay [`AtomDefinition`]s (typically from
    /// [`crate::hypothesized_proposal::promoted_proposal_to_atom_definition`]).
    ///
    /// Overlay atoms with an id that collides with a registry atom
    /// are dropped (registry wins), so a promotion can never silently
    /// shadow a production atom. The skip is logged at WARN with the
    /// colliding id so an SME-introduced node that accidentally
    /// reuses a registry-atom id surfaces in the operator logs.
    ///
    /// The returned registry is a fresh [`AtomRegistry`] — the
    /// receiver is left unchanged. Caller is responsible for using
    /// the returned registry in place of the base registry for the
    /// duration of the compose call.
    ///
    /// Unlike [`Self::load_from_dir`], this constructor does NOT
    /// schema-validate the overlay atoms (they are synthesized in
    /// Rust, not authored as YAML, and the schema check exists to
    /// catch authoring slips in human-written files). Callers that
    /// want overlay-id stability across releases should run
    /// [`Self::validate_consistency`] on the returned registry after
    /// construction — that pass catches structural invariants like
    /// dangling `depends_on` ids that apply equally to overlay atoms.
    pub fn with_promoted_overlay(&self, overlay: impl IntoIterator<Item = AtomDefinition>) -> Self {
        let mut atoms = self.atoms.clone();
        for atom in overlay {
            if atoms.contains_key(&atom.id) {
                tracing::warn!(
                    overlay_atom_id = %atom.id,
                    "AtomRegistry::with_promoted_overlay: overlay atom id collides with \
                     base registry atom; dropping overlay entry to preserve production atom"
                );
                continue;
            }
            atoms.insert(atom.id.clone(), atom);
        }
        Self { atoms }
    }

    fn compiled_schema() -> Result<&'static JSONSchema> {
        crate::schema_helpers::compile_schema_cached("atom", ATOM_SCHEMA_JSON)
    }

    /// Process-wide cached load. Subsequent calls with the same
    /// canonicalized `dir` return the same `Arc<AtomRegistry>` without
    /// re-reading or re-validating the YAML files. `load_from_dir`
    /// remains public for callers (tests, deterministic baselines) that
    /// need a fresh load.
    pub fn load_cached(dir: &Path) -> Result<Arc<Self>> {
        use std::collections::HashMap;
        use std::path::PathBuf;
        use std::sync::OnceLock;
        static CACHE: OnceLock<std::sync::Mutex<HashMap<PathBuf, Arc<AtomRegistry>>>> =
            OnceLock::new();
        let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        if let Ok(guard) = cache.lock() {
            if let Some(reg) = guard.get(&key) {
                return Ok(reg.clone());
            }
        }
        let reg = Arc::new(Self::load_from_dir(dir)?);
        if let Ok(mut guard) = cache.lock() {
            guard.insert(key, reg.clone());
        }
        Ok(reg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_atom(dir: &Path, name: &str, body: &str) {
        let p = dir.join(format!("{}.yaml", name));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn empty_dir_yields_empty_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = AtomRegistry::load_from_dir(tmp.path()).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn nonexistent_dir_yields_empty_registry() {
        let reg = AtomRegistry::load_from_dir(Path::new("/nonexistent/path")).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn loads_minimal_operation_atom() {
        let tmp = tempfile::tempdir().unwrap();
        write_atom(
            tmp.path(),
            "align_reads",
            r#"id: align_reads
version: "1.0.0"
role: operation
description: "Align short reads to a reference."
edam_operation: operation:0292
edam_data: data:2978
edam_format: format:2572
assignee: agent
"#,
        );
        let reg = AtomRegistry::load_from_dir(tmp.path()).unwrap();
        assert_eq!(reg.len(), 1);
        let atom = reg.get("align_reads").unwrap();
        assert_eq!(atom.id, "align_reads");
        assert_eq!(atom.edam_operation, "operation:0292");
    }

    #[test]
    fn rejects_id_stem_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        write_atom(
            tmp.path(),
            "wrong_stem",
            r#"id: align_reads
version: "1.0.0"
role: operation
description: "x"
edam_operation: operation:0292
assignee: agent
"#,
        );
        let err = AtomRegistry::load_from_dir(tmp.path())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("stem"),
            "expected stem-mismatch error, got: {}",
            err
        );
    }

    #[test]
    fn rejects_discovery_without_kind() {
        let tmp = tempfile::tempdir().unwrap();
        write_atom(
            tmp.path(),
            "discover_aligner",
            r#"id: discover_aligner
version: "1.0.0"
role: discovery
description: "x"
edam_operation: swfc:aligner_choice
assignee: agent
"#,
        );
        // Schema validation rejects role==discovery without
        // discovery_kind via the if/then arm.
        let err = AtomRegistry::load_from_dir(tmp.path())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("schema") || err.contains("discovery_kind"),
            "expected schema error, got: {}",
            err
        );
    }

    #[test]
    fn rejects_unknown_dependency() {
        let tmp = tempfile::tempdir().unwrap();
        write_atom(
            tmp.path(),
            "align_reads",
            r#"id: align_reads
version: "1.0.0"
role: operation
description: "x"
edam_operation: operation:0292
assignee: agent
depends_on: [data_acquisition]
"#,
        );
        let reg = AtomRegistry::load_from_dir(tmp.path()).unwrap();
        let err = reg.validate_consistency().unwrap_err().to_string();
        assert!(err.contains("unknown atom"), "got: {}", err);
    }

    /// `excludes` entries prefixed with `cel:` are
    /// CEL expressions and must compile at registry load.
    #[test]
    fn excludes_accepts_cel_prefix_and_compiles_at_load() {
        let tmp = tempfile::tempdir().unwrap();
        write_atom(
            tmp.path(),
            "human_only_atom",
            r#"id: human_only_atom
version: "1.0.0"
role: operation
description: "Excluded for non-human samples."
edam_operation: operation:0292
assignee: agent
excludes:
  - "cel:intake.organism.taxon_id != 9606"
"#,
        );
        let reg = AtomRegistry::load_from_dir(tmp.path()).unwrap();
        // A well-formed CEL exclusion must NOT trip the validator.
        reg.validate_consistency().expect("CEL exclusion compiles");
    }

    /// Empty CEL after the prefix is a load-time error
    /// so a YAML typo (`cel:` followed by accidental whitespace)
    /// fails fast instead of being treated as a single-token atom id.
    #[test]
    fn excludes_rejects_empty_cel_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        write_atom(
            tmp.path(),
            "broken_atom",
            r#"id: broken_atom
version: "1.0.0"
role: operation
description: "x"
edam_operation: operation:0292
assignee: agent
excludes:
  - "cel:   "
"#,
        );
        let reg = AtomRegistry::load_from_dir(tmp.path()).unwrap();
        let err = reg.validate_consistency().unwrap_err().to_string();
        assert!(
            err.contains("non-empty CEL expression"),
            "expected non-empty CEL error, got: {}",
            err
        );
    }

    /// Malformed CEL note. The cel-interpreter 0.10
    /// ANTLR-rust parser panics through antlr4rust on certain
    /// genuinely-malformed inputs (see `expression.rs::cel_evaluator_unbound_identifier_surfaces_error`
    /// for the documented workaround pattern). `Program::compile`
    /// rejects valid-but-typoed expressions cleanly; some sequences
    /// of stray operators trip the parser at a layer below our
    /// `?`-via-`Result` surface. The structural guarantee
    /// `validate_consistency` provides — well-formed CEL compiles, an
    /// empty `cel:` is rejected — is exercised by the two preceding
    /// tests. The narrower antlr-panic case is a cel-interpreter
    /// upstream bug; revisit when we either pin a future cel-rust
    /// release that fixes the antlr bridge, or swap the
    /// `ExpressionEvaluator` impl.
    #[test]
    #[ignore = "cel-interpreter 0.10 ANTLR parser panics rather than returning Err on certain malformed inputs; tracked for next-quarter cel-rust upgrade"]
    fn excludes_rejects_malformed_cel() {}

    #[test]
    fn iter_is_id_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = |id: &str| {
            format!(
                r#"id: {}
version: "1.0.0"
role: operation
description: "x"
edam_operation: operation:0004
assignee: agent
"#,
                id
            )
        };
        write_atom(tmp.path(), "zebra", &mk("zebra"));
        write_atom(tmp.path(), "alpha", &mk("alpha"));
        write_atom(tmp.path(), "mango", &mk("mango"));
        let reg = AtomRegistry::load_from_dir(tmp.path()).unwrap();
        let ids: Vec<&String> = reg.iter().map(|(k, _)| k).collect();
        assert_eq!(ids, vec!["alpha", "mango", "zebra"]);
    }

    #[test]
    fn find_producers_filters_by_edam_data() {
        let tmp = tempfile::tempdir().unwrap();
        let mk_with_data = |id: &str, data: &str| {
            format!(
                r#"id: {}
version: "1.0.0"
role: operation
description: "x"
edam_operation: operation:0004
edam_data: {}
edam_format: format:1929
assignee: agent
"#,
                id, data
            )
        };
        write_atom(
            tmp.path(),
            "produce_a",
            &mk_with_data("produce_a", "data:2978"),
        );
        write_atom(
            tmp.path(),
            "produce_b",
            &mk_with_data("produce_b", "data:1383"),
        );
        write_atom(
            tmp.path(),
            "produce_c",
            &mk_with_data("produce_c", "data:2978"),
        );
        let reg = AtomRegistry::load_from_dir(tmp.path()).unwrap();
        let hits: Vec<&str> = reg
            .find_producers("data:2978", None)
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(hits, vec!["produce_a", "produce_c"]);
    }

    /// `discover_axes()` is the source-of-truth for the BlockerCard
    /// auto-approve allowlist (server) and the e2e test helper's
    /// default. It must:
    ///   1. Include the atoms that carry `candidate_tools` (path 2).
    ///   2. Include the axis stems of atoms that carry
    ///      `method_choice.deferred_to` (path 1) — NOT the atom_id.
    ///   3. Exclude atoms with neither signal.
    ///   4. Cover the real proteomics axes (`peptide_search`,
    ///      `protein_quantification`) that the pre-fix hardcoded server
    ///      const was missing.
    #[test]
    fn discover_axes_covers_both_synthesis_paths() {
        let reg = AtomRegistry::load_from_dir(Path::new("../../config/stage-atoms"))
            .expect("load stage-atoms registry");
        let axes = reg.discover_axes();
        // Path 2 — candidate_tools-driven.
        for axis in [
            "alignment",
            "peptide_search",
            "protein_quantification",
            "differential_expression",
            "variant_calling",
            "peak_calling",
            "dimensionality_reduction",
        ] {
            assert!(
                axes.contains(axis),
                "discover_axes missing {axis:?}; got {} entries: {:?}",
                axes.len(),
                axes
            );
        }
        // Path 1 — method_choice axes (NOT the atom_id).
        for axis in [
            "dtu_method",                // <- differential_transcript_usage
            "isoform_caller",            // <- isoform_discovery
            "spatial_clustering_method", // <- spatial_domain_segmentation
            "time_series_method",        // <- time_series_model_fitting
        ] {
            assert!(
                axes.contains(axis),
                "discover_axes missing method_choice axis {axis:?}; got {:?}",
                axes
            );
        }
        // The original atoms that defer to those axes must NOT
        // themselves appear — `method_choice` wins over `candidate_tools`.
        for not_present in [
            "differential_transcript_usage",
            "isoform_discovery",
            "spatial_domain_segmentation",
            "time_series_model_fitting",
        ] {
            assert!(
                !axes.contains(not_present),
                "discover_axes wrongly includes atom_id {not_present:?} \
                 — method_choice axis should win"
            );
        }
        // Sanity bound — the registry holds ~80 atoms; should produce
        // 40+ axes after both paths. Catches accidental short-circuits.
        assert!(
            axes.len() >= 40,
            "discover_axes produced only {} entries; expected 40+; got {:?}",
            axes.len(),
            axes
        );
    }

    /// Empty registry → empty axes (no panic, no crash).
    #[test]
    fn discover_axes_empty_when_registry_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = AtomRegistry::load_from_dir(tmp.path()).unwrap();
        assert!(reg.discover_axes().is_empty());
    }
}
