//! `AtomCatalogDiff` — added / removed / version-bumped atoms between two
//! catalog snapshots. Deterministic (sorted vecs, `BTreeMap`-keyed atoms),
//! `#[non_exhaustive]` for the wire-facing SemVer contract.
//!
//! The catalog (atoms + archetypes) is the unit of provenance + evolution:
//! versioning discipline, diffing, and per-atom contract linting all live
//! here so registry-lifecycle tooling has one home.

use crate::atom::AtomDefinition;
use crate::atom_registry::{semver_cmp, AtomRegistry};
use serde::{Deserialize, Serialize};
use std::path::Path;
use ts_rs::TS;

/// A single atom whose version changed between two catalogs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct VersionBump {
    /// Atom id.
    pub id: String,
    /// Version in the old catalog.
    pub from: String,
    /// Version in the new catalog.
    pub to: String,
}

/// Diff between two atom-catalog snapshots. Added/removed/bumped lists are
/// id-sorted for byte-stable output.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[non_exhaustive]
pub struct AtomCatalogDiff {
    /// Ids present in `new` but not `old`.
    pub added: Vec<String>,
    /// Ids present in `old` but not `new`.
    pub removed: Vec<String>,
    /// Ids present in both whose version changed (any direction).
    pub version_bumped: Vec<VersionBump>,
}

impl AtomCatalogDiff {
    /// Compute the diff `old → new`. Pure; deterministic.
    pub fn between(old: &AtomRegistry, new: &AtomRegistry) -> Self {
        use std::collections::BTreeMap;
        let old_map: BTreeMap<&str, &str> = old
            .iter()
            .map(|(id, a)| (id.as_str(), a.version.as_str()))
            .collect();
        let new_map: BTreeMap<&str, &str> = new
            .iter()
            .map(|(id, a)| (id.as_str(), a.version.as_str()))
            .collect();

        let added: Vec<String> = new_map
            .keys()
            .filter(|id| !old_map.contains_key(*id))
            .map(|id| id.to_string())
            .collect();
        let removed: Vec<String> = old_map
            .keys()
            .filter(|id| !new_map.contains_key(*id))
            .map(|id| id.to_string())
            .collect();
        let version_bumped: Vec<VersionBump> = old_map
            .iter()
            .filter_map(|(id, ov)| {
                new_map.get(id).and_then(|nv| {
                    if semver_cmp(ov, nv) != std::cmp::Ordering::Equal {
                        Some(VersionBump {
                            id: id.to_string(),
                            from: ov.to_string(),
                            to: nv.to_string(),
                        })
                    } else {
                        None
                    }
                })
            })
            .collect();
        Self {
            added,
            removed,
            version_bumped,
        }
    }

    /// True when any atom was added, removed, or version-bumped.
    pub fn has_changes(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty() || !self.version_bumped.is_empty()
    }
}

/// Run the per-atom contract lint over a registry. Returns one
/// human-readable string per violation (empty = clean). Three checks:
///
/// 1. **serde round-trip** — the atom serializes and re-deserializes to an
///    equal value (catches a field that won't survive emission).
/// 2. **figure-affordance** — delegated to the SAME `check_atom`
///    figure-obligation resolver the `figure_obligation` test gate uses
///    (handles exact match, declared-parents + registry BFS-ancestor
///    walk, `figure_exempt`, adapter detection, and the `required_figures`
///    subset check). We never reimplement the BFS here.
/// 3. **non-orphan ports** — every atom declaring `depends_on` resolves
///    those ids in the registry.
///
/// `affordance_dir` is the directory holding `registered.yaml`
/// (`config/plot-affordances/`). When the affordance registry cannot be
/// loaded, the figure check is skipped (the serde + non-orphan checks
/// still run) and a single load-failure line is recorded.
pub fn lint_atom_contracts(reg: &AtomRegistry, affordance_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    // Reuse the existing affordance registry loader so this gate and the
    // figure_obligation test gate share one source of truth.
    let affordances =
        crate::plot_affordance::registry::YamlPlotAffordanceRegistry::from_dir(affordance_dir);
    let affordances = match affordances {
        Ok(r) => Some(r),
        Err(e) => {
            out.push(format!(
                "plot-affordance registry failed to load from {}: {e}; figure check skipped",
                affordance_dir.display()
            ));
            None
        }
    };

    for (id, atom) in reg.iter() {
        // 1. serde round-trip.
        match serde_json::to_value(atom).and_then(serde_json::from_value::<AtomDefinition>) {
            Ok(back) if &back == atom => {}
            Ok(_) => out.push(format!("{id}: serde round-trip changed the atom value")),
            Err(e) => out.push(format!("{id}: serde round-trip failed: {e}")),
        }
        // 2. figure-affordance — delegate to the shared check_atom resolver.
        if let Some(aff) = affordances.as_ref() {
            if let Err(v) = crate::plot_affordance::check_atom(atom, aff, "theme.json") {
                out.push(format!("{id}: {}", v.reason));
            }
        }
        // 3. non-orphan depends_on.
        for dep in &atom.depends_on {
            if reg.get(dep).is_none() {
                out.push(format!("{id}: depends_on references unknown atom `{dep}`"));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom_registry::AtomRegistry;
    use std::io::Write;
    use std::path::Path;

    fn write_atom(dir: &Path, id: &str, version: &str) {
        let body = format!(
            "id: {id}\nversion: \"{version}\"\nrole: operation\ndescription: x\nedam_operation: operation:0004\nassignee: agent\n"
        );
        let mut f = std::fs::File::create(dir.join(format!("{id}.yaml"))).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn diff_classifies_added_removed_bumped() {
        let old_dir = tempfile::tempdir().unwrap();
        write_atom(old_dir.path(), "keep_same", "1.0.0");
        write_atom(old_dir.path(), "will_remove", "1.0.0");
        write_atom(old_dir.path(), "will_bump", "1.0.0");
        let new_dir = tempfile::tempdir().unwrap();
        write_atom(new_dir.path(), "keep_same", "1.0.0");
        write_atom(new_dir.path(), "will_bump", "1.1.0");
        write_atom(new_dir.path(), "newly_added", "0.1.0");

        let old = AtomRegistry::load_from_dir(old_dir.path()).unwrap();
        let new = AtomRegistry::load_from_dir(new_dir.path()).unwrap();
        let diff = AtomCatalogDiff::between(&old, &new);

        assert_eq!(diff.added, vec!["newly_added".to_string()]);
        assert_eq!(diff.removed, vec!["will_remove".to_string()]);
        assert_eq!(
            diff.version_bumped,
            vec![VersionBump {
                id: "will_bump".into(),
                from: "1.0.0".into(),
                to: "1.1.0".into()
            }]
        );
        assert!(diff.has_changes());
    }

    #[test]
    fn identical_catalogs_have_no_changes() {
        let dir = tempfile::tempdir().unwrap();
        write_atom(dir.path(), "only_atom", "1.0.0");
        let reg = AtomRegistry::load_from_dir(dir.path()).unwrap();
        let diff = AtomCatalogDiff::between(&reg, &reg);
        assert!(!diff.has_changes());
    }
}
