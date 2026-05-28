//! Multi-AZ failover — when `AwsExecutor::provision` hits
//! `InsufficientInstanceCapacity` in one subnet, it rotates to the
//! next subnet in `SWFC_AWS_SUBNET_IDS` (comma-separated) and
//! retries.

use std::env;

/// Parses `SWFC_AWS_SUBNET_IDS` as a comma-separated list. Falls
/// back to the single-subnet `SWFC_AWS_SUBNET_ID` when the plural
/// form is unset. Empty result means "no subnet configured" — the
/// caller should refuse to provision.
pub fn subnet_rotation() -> Vec<String> {
    if let Ok(raw) = env::var("SWFC_AWS_SUBNET_IDS") {
        return parse_list(&raw);
    }
    if let Ok(single) = env::var("SWFC_AWS_SUBNET_ID") {
        let t = single.trim();
        if !t.is_empty() {
            return vec![t.to_string()];
        }
    }
    Vec::new()
}

fn parse_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Simple round-robin cursor over a subnet rotation. The caller
/// starts at index 0 and advances on `InsufficientInstanceCapacity`;
/// the cursor wraps so a long-running harness doesn't stall after
/// exhausting the list once.
#[derive(Debug, Clone)]
pub struct SubnetCursor {
    subnets: Vec<String>,
    next: usize,
}

impl SubnetCursor {
    /// Creates a cursor from an explicit subnet list starting at index 0.
    pub fn new(subnets: Vec<String>) -> Self {
        Self { subnets, next: 0 }
    }

    /// Creates a cursor from `SWFC_AWS_SUBNET_IDS` (or `SWFC_AWS_SUBNET_ID`).
    pub fn from_env() -> Self {
        Self::new(subnet_rotation())
    }

    /// Returns the number of subnets in the rotation.
    pub fn len(&self) -> usize {
        self.subnets.len()
    }

    /// Returns `true` when no subnets are configured.
    pub fn is_empty(&self) -> bool {
        self.subnets.is_empty()
    }

    /// Return the next subnet id and advance the cursor. Returns
    /// None when no subnets are configured.
    pub fn advance(&mut self) -> Option<String> {
        if self.subnets.is_empty() {
            return None;
        }
        let idx = self.next % self.subnets.len();
        self.next = self.next.wrapping_add(1);
        Some(self.subnets[idx].clone())
    }

    /// Snapshot of the rotation without advancing — useful for
    /// logging the full list in an error message.
    pub fn all(&self) -> &[String] {
        &self.subnets
    }
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup; bounded waiver scoped to this
    // `mod tests` block.
    #![allow(unsafe_code)]
    use super::*;

    #[test]
    fn parse_list_splits_and_trims() {
        assert_eq!(
            parse_list("subnet-a, subnet-b,subnet-c"),
            vec!["subnet-a", "subnet-b", "subnet-c"]
        );
    }

    #[test]
    fn parse_list_drops_empty_entries() {
        assert_eq!(
            parse_list("subnet-a,,subnet-b,  "),
            vec!["subnet-a", "subnet-b"]
        );
    }

    #[test]
    fn cursor_rotates_round_robin() {
        let mut c = SubnetCursor::new(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(c.advance().as_deref(), Some("a"));
        assert_eq!(c.advance().as_deref(), Some("b"));
        assert_eq!(c.advance().as_deref(), Some("c"));
        // Wraps.
        assert_eq!(c.advance().as_deref(), Some("a"));
    }

    #[test]
    fn cursor_empty_returns_none() {
        let mut c = SubnetCursor::new(vec![]);
        assert!(c.advance().is_none());
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
    }

    fn with_env<T>(plural: Option<&str>, singular: Option<&str>, body: impl FnOnce() -> T) -> T {
        // Serialize with the aws.rs env tests via the crate-wide
        // SWFC_AWS_ENV_LOCK so we don't race their transient
        // SWFC_AWS_SUBNET_IDS=subnet-test overrides.
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prior_plural = env::var("SWFC_AWS_SUBNET_IDS").ok();
        let prior_single = env::var("SWFC_AWS_SUBNET_ID").ok();
        match plural {
            Some(v) => unsafe { env::set_var("SWFC_AWS_SUBNET_IDS", v) },
            None => unsafe { env::remove_var("SWFC_AWS_SUBNET_IDS") },
        }
        match singular {
            Some(v) => unsafe { env::set_var("SWFC_AWS_SUBNET_ID", v) },
            None => unsafe { env::remove_var("SWFC_AWS_SUBNET_ID") },
        }
        let out = body();
        match prior_plural {
            Some(v) => unsafe { env::set_var("SWFC_AWS_SUBNET_IDS", v) },
            None => unsafe { env::remove_var("SWFC_AWS_SUBNET_IDS") },
        }
        match prior_single {
            Some(v) => unsafe { env::set_var("SWFC_AWS_SUBNET_ID", v) },
            None => unsafe { env::remove_var("SWFC_AWS_SUBNET_ID") },
        }
        out
    }

    #[test]
    fn from_env_prefers_plural_over_singular() {
        with_env(Some("subnet-a,subnet-b"), Some("subnet-legacy"), || {
            let c = SubnetCursor::from_env();
            assert_eq!(c.len(), 2);
            assert_eq!(c.all(), &["subnet-a".to_string(), "subnet-b".to_string()]);
        });
    }

    #[test]
    fn from_env_falls_back_to_singular() {
        with_env(None, Some("subnet-solo"), || {
            let c = SubnetCursor::from_env();
            assert_eq!(c.len(), 1);
            assert_eq!(c.all(), &["subnet-solo".to_string()]);
        });
    }

    #[test]
    fn from_env_unset_is_empty() {
        with_env(None, None, || {
            let c = SubnetCursor::from_env();
            assert!(c.is_empty());
        });
    }
}
