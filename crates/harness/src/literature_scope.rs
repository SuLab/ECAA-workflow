//! Phase D of the literature-atom plan — typed reader for the four
//! SWFC_LIT_* env vars that gate literature retrieval scope, NCBI rate
//! limits, evidence storage caps, and institutional-access opt-in.
//! Invalid values fall back to safe defaults with a tracing warning
//! (per the SWFC_HARNESS_BATCH_WINDOW_SECS precedent).
//!
//! The agent helper (`scripts/agent_literature_fetch.py`) reads these
//! env vars from its environment at task-execution time; the harness
//! injects them via `LiteratureScopeConfig::agent_env_vars` when
//! spawning the agent subprocess (Task 6).

use std::env;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Selects which literature sources the agent fetches during evidence retrieval.
pub enum LiteratureScope {
    /// Open-Access PMC full text only (default).
    PmcOa,
    /// PMC Open-Access full text plus abstract-level records for all PubMed entries.
    PmcOaPlusAbstracts,
    /// All sources, but restricted to locally cached copies (no outbound NCBI calls).
    AllSourcesLocalOnly,
}

impl LiteratureScope {
    /// Returns the canonical `SWFC_LIT_SOURCE_SCOPE` string for this variant.
    pub fn as_env_str(self) -> &'static str {
        match self {
            Self::PmcOa => "pmc_oa",
            Self::PmcOaPlusAbstracts => "pmc_oa_plus_abstracts",
            Self::AllSourcesLocalOnly => "all_sources_local_only",
        }
    }
    /// Parses a `SWFC_LIT_SOURCE_SCOPE` string. Returns `None` on unrecognised values.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pmc_oa" => Some(Self::PmcOa),
            "pmc_oa_plus_abstracts" => Some(Self::PmcOaPlusAbstracts),
            "all_sources_local_only" => Some(Self::AllSourcesLocalOnly),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
/// Typed configuration for the four `SWFC_LIT_*` environment variables.
pub struct LiteratureScopeConfig {
    /// Which literature sources to fetch from.
    pub scope: LiteratureScope,
    /// Optional NCBI E-utilities API key (`SWFC_LIT_NCBI_API_KEY`). Absent means 3 req/s rate limit.
    pub ncbi_api_key: Option<String>,
    /// Per-task evidence storage cap in MiB (`SWFC_LIT_EVIDENCE_MAX_MB`, default 200).
    pub evidence_max_mb: u64,
    /// When `true`, enables institutional-access paths (`SWFC_LIT_INSTITUTIONAL_ACCESS=1`).
    /// Only effective with `AllSourcesLocalOnly` scope.
    pub institutional_access: bool,
}

impl LiteratureScopeConfig {
    /// Reads all `SWFC_LIT_*` env vars and constructs the config. Invalid values fall back to safe defaults.
    pub fn from_env() -> Self {
        let raw_scope = env::var("SWFC_LIT_SOURCE_SCOPE").ok();
        let scope = match raw_scope.as_deref() {
            None => LiteratureScope::PmcOa,
            Some(s) => match LiteratureScope::parse(s) {
                Some(v) => v,
                None => {
                    tracing::warn!(
                        "invalid SWFC_LIT_SOURCE_SCOPE={}; falling back to pmc_oa",
                        s
                    );
                    LiteratureScope::PmcOa
                }
            },
        };
        let ncbi_api_key = env::var("SWFC_LIT_NCBI_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        let evidence_max_mb = env::var("SWFC_LIT_EVIDENCE_MAX_MB")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(200);
        let institutional_access = env::var("SWFC_LIT_INSTITUTIONAL_ACCESS")
            .map(|v| v == "1")
            .unwrap_or(false);

        if institutional_access && scope != LiteratureScope::AllSourcesLocalOnly {
            tracing::warn!(
                "SWFC_LIT_INSTITUTIONAL_ACCESS=1 but scope is {}; institutional_access ignored",
                scope.as_env_str()
            );
        }

        Self {
            scope,
            ncbi_api_key,
            evidence_max_mb,
            institutional_access,
        }
    }

    /// Returns the `SWFC_LIT_*` env-var key-value pairs to inject into the agent subprocess.
    pub fn agent_env_vars(&self) -> Vec<(String, String)> {
        let mut vars = vec![
            (
                "SWFC_LIT_SOURCE_SCOPE".into(),
                self.scope.as_env_str().to_string(),
            ),
            (
                "SWFC_LIT_EVIDENCE_MAX_MB".into(),
                self.evidence_max_mb.to_string(),
            ),
        ];
        if let Some(k) = &self.ncbi_api_key {
            vars.push(("SWFC_LIT_NCBI_API_KEY".into(), k.clone()));
        }
        if self.institutional_access && self.scope == LiteratureScope::AllSourcesLocalOnly {
            vars.push(("SWFC_LIT_INSTITUTIONAL_ACCESS".into(), "1".into()));
        }
        vars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F) {
        let restore: Vec<_> = vars.iter().map(|(k, _)| (*k, env::var(k).ok())).collect();
        for (k, v) in vars {
            match v {
                Some(v) => env::set_var(k, v),
                None => env::remove_var(k),
            }
        }
        f();
        for (k, v) in restore {
            match v {
                Some(v) => env::set_var(k, v),
                None => env::remove_var(k),
            }
        }
    }

    #[test]
    fn defaults_to_pmc_oa() {
        with_env(
            &[
                ("SWFC_LIT_SOURCE_SCOPE", None),
                ("SWFC_LIT_NCBI_API_KEY", None),
                ("SWFC_LIT_EVIDENCE_MAX_MB", None),
                ("SWFC_LIT_INSTITUTIONAL_ACCESS", None),
            ],
            || {
                let cfg = LiteratureScopeConfig::from_env();
                assert_eq!(cfg.scope, LiteratureScope::PmcOa);
                assert_eq!(cfg.evidence_max_mb, 200);
                assert!(cfg.ncbi_api_key.is_none());
                assert!(!cfg.institutional_access);
            },
        );
    }

    #[test]
    fn invalid_scope_falls_back_to_default() {
        with_env(&[("SWFC_LIT_SOURCE_SCOPE", Some("bogus"))], || {
            assert_eq!(
                LiteratureScopeConfig::from_env().scope,
                LiteratureScope::PmcOa
            );
        });
    }

    #[test]
    fn agent_env_vars_round_trip() {
        with_env(
            &[
                ("SWFC_LIT_SOURCE_SCOPE", Some("pmc_oa_plus_abstracts")),
                ("SWFC_LIT_NCBI_API_KEY", Some("test_key_value")),
                ("SWFC_LIT_EVIDENCE_MAX_MB", Some("500")),
            ],
            || {
                let cfg = LiteratureScopeConfig::from_env();
                let vars: std::collections::HashMap<_, _> =
                    cfg.agent_env_vars().into_iter().collect();
                assert_eq!(
                    vars.get("SWFC_LIT_SOURCE_SCOPE").unwrap(),
                    "pmc_oa_plus_abstracts"
                );
                assert_eq!(vars.get("SWFC_LIT_NCBI_API_KEY").unwrap(), "test_key_value");
                assert_eq!(vars.get("SWFC_LIT_EVIDENCE_MAX_MB").unwrap(), "500");
            },
        );
    }

    #[test]
    fn institutional_access_ignored_outside_local_scope() {
        with_env(
            &[
                ("SWFC_LIT_SOURCE_SCOPE", Some("pmc_oa")),
                ("SWFC_LIT_INSTITUTIONAL_ACCESS", Some("1")),
            ],
            || {
                let cfg = LiteratureScopeConfig::from_env();
                let vars: std::collections::HashMap<_, _> =
                    cfg.agent_env_vars().into_iter().collect();
                // institutional_access reads as true on the config struct
                // but is NOT emitted into agent_env_vars when the scope
                // disagrees (gated by the warning at from_env).
                assert!(cfg.institutional_access);
                assert!(!vars.contains_key("SWFC_LIT_INSTITUTIONAL_ACCESS"));
            },
        );
    }

    #[test]
    fn institutional_access_emitted_under_correct_scope() {
        with_env(
            &[
                ("SWFC_LIT_SOURCE_SCOPE", Some("all_sources_local_only")),
                ("SWFC_LIT_INSTITUTIONAL_ACCESS", Some("1")),
            ],
            || {
                let cfg = LiteratureScopeConfig::from_env();
                let vars: std::collections::HashMap<_, _> =
                    cfg.agent_env_vars().into_iter().collect();
                assert_eq!(vars.get("SWFC_LIT_INSTITUTIONAL_ACCESS").unwrap(), "1");
            },
        );
    }
}
