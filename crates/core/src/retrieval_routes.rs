//! Typed loader for `config/downstream-policy/retrieval-routes.json` — maps a
//! source class to the concrete hosts/domain-suffixes the agent may fetch
//! from. Used to (a) build the survey atom's egress allowlist and (b) bound
//! the agent's literature fetcher.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Concrete fetch targets for one source class: exact `hosts` plus optional
/// `domainSuffixes` (e.g. `.readthedocs.io`) that bound a subdomain family.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassRoute {
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub domain_suffixes: Vec<String>,
}

/// Source-class → concrete fetch targets, loaded from
/// `retrieval-routes.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievalRoutes {
    pub schema_version: String,
    pub routes_by_class: BTreeMap<String, ClassRoute>,
}

impl RetrievalRoutes {
    /// Load + parse the routes config from `path`.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Exact hosts declared for `class` (empty if the class is absent).
    pub fn hosts_for_class(&self, class: &str) -> Vec<String> {
        self.routes_by_class
            .get(class)
            .map(|r| r.hosts.clone())
            .unwrap_or_default()
    }

    /// Domain suffixes declared for `class` (empty if absent).
    pub fn domain_suffixes_for_class(&self, class: &str) -> Vec<String> {
        self.routes_by_class
            .get(class)
            .map(|r| r.domain_suffixes.clone())
            .unwrap_or_default()
    }

    /// Every exact host across all classes, sorted and deduplicated.
    pub fn all_hosts(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .routes_by_class
            .values()
            .flat_map(|r| r.hosts.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routes_path() -> std::path::PathBuf {
        Path::new("../../config/downstream-policy/retrieval-routes.json").to_path_buf()
    }

    #[test]
    fn loads_routes_and_flattens_allowlist() {
        let routes = RetrievalRoutes::load(&routes_path()).unwrap();
        let hosts = routes.all_hosts();
        assert!(
            hosts.contains(&"api.openalex.org".to_string()),
            "all_hosts must contain api.openalex.org, got {hosts:?}"
        );
        assert!(
            hosts.contains(&"eutils.ncbi.nlm.nih.gov".to_string()),
            "all_hosts must contain eutils.ncbi.nlm.nih.gov, got {hosts:?}"
        );
        assert!(
            routes
                .hosts_for_class("tool_documentation")
                .iter()
                .any(|h| h.contains("readthedocs")),
            "tool_documentation hosts must include a readthedocs entry"
        );
    }

    #[test]
    fn all_hosts_is_sorted_and_deduped() {
        let routes = RetrievalRoutes::load(&routes_path()).unwrap();
        let hosts = routes.all_hosts();
        let mut sorted = hosts.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(hosts, sorted, "all_hosts must be sorted and deduped");
    }

    #[test]
    fn tool_documentation_carries_domain_suffixes() {
        let routes = RetrievalRoutes::load(&routes_path()).unwrap();
        let suffixes = routes.domain_suffixes_for_class("tool_documentation");
        assert!(
            suffixes.iter().any(|s| s == ".readthedocs.io"),
            "tool_documentation domainSuffixes must include .readthedocs.io, got {suffixes:?}"
        );
    }

    #[test]
    fn class_routes_present_for_each_source_class() {
        let routes = RetrievalRoutes::load(&routes_path()).unwrap();
        for class in [
            "primary_literature",
            "conference_proceedings",
            "tool_documentation",
        ] {
            assert!(
                !routes.hosts_for_class(class).is_empty(),
                "class {class} must declare at least one host"
            );
        }
        assert_eq!(routes.schema_version, "1.0");
    }
}
