use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::affordance::PlotAffordance;

/// Registry surface the selector consumes. The default impl is
/// `YamlPlotAffordanceRegistry` which loads from
/// `config/plot-affordances/*.yaml`. Tests can substitute an
/// `InMemoryPlotAffordanceRegistry`.
pub trait PlotAffordanceRegistry: Send + Sync {
    /// Returns every registered affordance keyed by exact
    /// SemanticType IRI (e.g., `EDAM:data_3134`).
    fn lookup_exact(&self, semantic_type: &str) -> Option<&RegisteredAffordance>;

    /// Returns parent-term IRIs in subsumption order
    /// (closer-to-`SemanticType` first). Used by the selector's
    /// `InheritedViaOntology` branch.
    fn parents_of(&self, semantic_type: &str) -> Vec<String>;

    /// Stable snapshot id pinned to the registry's content digest.
    /// Required for `AffordanceProof.registry_snapshot_id`.
    fn snapshot_id(&self) -> &str;

    /// Iterate every registered affordance, sorted by SemanticType
    /// IRI. Used by the catalog-completeness audit and the shadowing
    /// check in `propose_hypothesized_renderer`.
    fn iter(&self) -> Box<dyn Iterator<Item = (&str, &RegisteredAffordance)> + '_>;

    /// Returns `true` when any registered affordance lists the given
    /// `figure_id` in its `figure_ids`. Default impl walks `iter()`;
    /// implementations may override with an indexed lookup.
    fn is_registered_figure_id(&self, figure_id: &str) -> bool {
        self.iter()
            .any(|(_, reg)| reg.figure_ids.iter().any(|f| f == figure_id))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
/// RegisteredAffordance data.
pub struct RegisteredAffordance {
    /// Semantic type.
    pub semantic_type: String,
    /// Figure ids.
    pub figure_ids: Vec<String>,
    /// Renderer module.
    pub renderer_module: String,
    /// Theme version.
    pub theme_version: String,
}

/// YamlPlotAffordanceRegistry data.
pub struct YamlPlotAffordanceRegistry {
    by_semantic_type: BTreeMap<String, RegisteredAffordance>,
    parents: BTreeMap<String, Vec<String>>,
    snapshot_id: String,
}

/// Flat entry type used to construct a `YamlPlotAffordanceRegistry`
/// in-memory (e.g. in unit tests) without reading YAML files.
#[derive(Clone, Debug)]
pub struct PlotAffordanceEntry {
    /// Semantic type.
    pub semantic_type: String,
    /// Figure ids.
    pub figure_ids: Vec<String>,
    /// Renderer module.
    pub renderer_module: String,
    /// Theme version.
    pub theme_version: String,
    /// Parents.
    pub parents: Vec<String>,
}

impl YamlPlotAffordanceRegistry {
    /// Construct an empty registry. Useful in unit tests that want to
    /// assert `Deferred` outcomes without loading any YAML files.
    pub fn empty() -> Self {
        Self {
            by_semantic_type: BTreeMap::new(),
            parents: BTreeMap::new(),
            snapshot_id: "empty".into(),
        }
    }

    /// Construct a registry from a vec of `PlotAffordanceEntry` values.
    /// Useful in unit tests that need specific affordances registered
    /// without loading YAML files from disk.
    pub fn from_entries(entries: Vec<PlotAffordanceEntry>) -> Self {
        let mut by_semantic_type: BTreeMap<String, RegisteredAffordance> = BTreeMap::new();
        let mut parents: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for entry in entries {
            parents.insert(entry.semantic_type.clone(), entry.parents);
            by_semantic_type.insert(
                entry.semantic_type.clone(),
                RegisteredAffordance {
                    semantic_type: entry.semantic_type,
                    figure_ids: entry.figure_ids,
                    renderer_module: entry.renderer_module,
                    theme_version: entry.theme_version,
                },
            );
        }

        let canonical = serde_json::to_string(&by_semantic_type).unwrap_or_else(|_| "{}".into());
        let snapshot_id = crate::hash_utils::sha256_short(canonical.as_bytes(), 16);

        Self {
            by_semantic_type,
            parents,
            snapshot_id,
        }
    }

    /// From dir.
    pub fn from_dir(dir: &Path) -> std::io::Result<Self> {
        let mut by_semantic_type: BTreeMap<String, RegisteredAffordance> = BTreeMap::new();
        let mut parents: BTreeMap<String, Vec<String>> = BTreeMap::new();

        // `std::fs::read_dir` returns entries in filesystem order
        // (typically inode order; tmpfs + ext4 differ; CI vs dev
        // workstation differ). When two YAML files declare the same
        // `semantic_type` the last-inserted entry wins, so an unsorted
        // iteration breaks byte-determinism (which
        // `snapshot_id = sha256(canonical_json)` depends on load-
        // bearingly). Collect → sort by path → iterate so the merge
        // order is byte-stable across hosts. Mirrors the pattern in
        // `adapter_registry::merge_yaml_dir`.
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.starts_with('_'))
                .unwrap_or(false)
            {
                continue; // skip schema sidecars
            }
            let body = std::fs::read_to_string(&path)?;
            let file: RegisteredAffordancesFile = serde_yml::from_str(&body)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            for entry in file.affordances {
                parents.insert(entry.semantic_type.clone(), entry.parents.clone());
                by_semantic_type.insert(
                    entry.semantic_type.clone(),
                    RegisteredAffordance {
                        semantic_type: entry.semantic_type,
                        figure_ids: entry.figure_ids,
                        renderer_module: entry.renderer_module,
                        theme_version: entry.theme_version,
                    },
                );
            }
        }

        let canonical = serde_json::to_string(&by_semantic_type).map_err(std::io::Error::other)?;
        let snapshot_id = crate::hash_utils::sha256_short(canonical.as_bytes(), 16);

        Ok(Self {
            by_semantic_type,
            parents,
            snapshot_id,
        })
    }
}

#[derive(Deserialize)]
struct RegisteredAffordancesFile {
    affordances: Vec<RegisteredAffordanceYaml>,
}

#[derive(Deserialize)]
struct RegisteredAffordanceYaml {
    semantic_type: String,
    figure_ids: Vec<String>,
    renderer_module: String,
    theme_version: String,
    #[serde(default)]
    parents: Vec<String>,
}

impl PlotAffordanceRegistry for YamlPlotAffordanceRegistry {
    fn lookup_exact(&self, semantic_type: &str) -> Option<&RegisteredAffordance> {
        self.by_semantic_type.get(semantic_type)
    }

    fn parents_of(&self, semantic_type: &str) -> Vec<String> {
        self.parents.get(semantic_type).cloned().unwrap_or_default()
    }

    fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (&str, &RegisteredAffordance)> + '_> {
        Box::new(self.by_semantic_type.iter().map(|(k, v)| (k.as_str(), v)))
    }
}

/// Blanket impl so callers that box the registry trait object
/// (`Box<dyn PlotAffordanceRegistry>`) can pass it directly to
/// generic functions that take `R: PlotAffordanceRegistry`.
///
/// All methods simply delegate to the inner `dyn PlotAffordanceRegistry`.
/// Used by the Phase A1–A3 emit-time resolver integration in
/// `crates/conversation::emit::audit_log`.
impl PlotAffordanceRegistry for Box<dyn PlotAffordanceRegistry> {
    fn lookup_exact(&self, semantic_type: &str) -> Option<&RegisteredAffordance> {
        self.as_ref().lookup_exact(semantic_type)
    }

    fn parents_of(&self, semantic_type: &str) -> Vec<String> {
        self.as_ref().parents_of(semantic_type)
    }

    fn snapshot_id(&self) -> &str {
        self.as_ref().snapshot_id()
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (&str, &RegisteredAffordance)> + '_> {
        self.as_ref().iter()
    }
}

/// `_ = PlotAffordance` keeps the symbol reachable for downstream tests.
#[allow(dead_code)]
fn _ensure_re_export_compiles(a: PlotAffordance) -> PlotAffordance {
    a
}
