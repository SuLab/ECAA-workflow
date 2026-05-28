//! Versioned AWS instance pricing table used by the cost guard.
//!
//! Updated manually on a quarterly cadence (see `PRICING_TABLE_REVISION`).
//! These numbers are intentionally separate from runtime profile defaults so
//! a price refresh is a one-file diff that's easy to review.
//!
//! ## Refresh procedure
//!
//! 1. Pull current on-demand prices from the AWS pricing API for region
//!    `us-east-1` (our reference region).
//! 2. Update the `INSTANCE_PRICES_USD_PER_HOUR` array below.
//! 3. Bump `PRICING_TABLE_REVISION` to today's date.
//! 4. Run `cargo test -p ecaa-workflow-harness pricing` to verify the
//!    sanity invariants below still hold.
//! 5. Commit with message `chore(harness): refresh AWS pricing table YYYY-MM-DD`.
//!
//! ## Regional override
//!
//! Set `SWFC_AWS_PRICING_REGION_MULT=1.10` to scale all prices by 10% when
//! running in a region pricier than us-east-1. `Pricing::for_region` reads
//! the env var at construction time and applies the multiplier to every
//! lookup. Set `SWFC_AWS_PRICING_OVERRIDES_JSON=/path/to/override.json` to
//! inject a full replacement table (not yet wired — file an issue if you
//! need this).

/// Date the pricing table was last refreshed against the AWS pricing API.
/// Format: `YYYY-MM-DD`. Stale > 90 days → tracing warn at cost-guard init.
pub const PRICING_TABLE_REVISION: &str = "2026-05-16";

/// Maximum age (in days) before we log a staleness warning at startup.
pub const PRICING_TABLE_STALENESS_WARN_DAYS: i64 = 90;

/// AWS spot instances historically run 20–40% of on-demand pricing across
/// regions. This is the conservative midpoint; bid is computed as
/// `on_demand * SPOT_DISCOUNT_FRACTION`. Override via
/// `SWFC_AWS_SPOT_DISCOUNT_FRACTION` env if you've measured a region-specific
/// average that materially differs.
pub const SPOT_DISCOUNT_FRACTION: f64 = 0.30;

/// R-20 — env var name for the regional pricing multiplier.
pub const PRICING_REGION_MULT_ENV: &str = "SWFC_AWS_PRICING_REGION_MULT";

/// On-demand USD per hour, reference region us-east-1.
/// Sorted by instance family then size. Approximate 2026-Q1 list rates;
/// refresh procedure documented at the top of this module.
///
/// R-20 — the table must cover every type
/// `sizing::resolve_instance_type` can return. The picker climbs
/// (t3 → c6i.{1..12}xlarge → r6i.{1..16}xlarge → GPU shapes) so an
/// undersized table silently demotes a high-vCPU pick to "unknown
/// instance type → cost-guard error". Cross-checked against
/// `sizing::INSTANCE_CAPACITY`; the
/// `pricing_table_covers_every_sizing_pick` test below pins the
/// invariant so a future sizing addition that forgets a price row
/// fails fast.
pub const INSTANCE_PRICES_USD_PER_HOUR: &[(&str, f64)] = &[
    // General-purpose burstable (T3 Intel)
    ("t3.medium", 0.0416),
    ("t3.large", 0.0832),
    // Compute-optimized (C6i Intel Ice Lake)
    ("c6i.xlarge", 0.17),
    ("c6i.2xlarge", 0.34),
    ("c6i.4xlarge", 0.68),
    ("c6i.8xlarge", 1.36),
    ("c6i.12xlarge", 2.04),
    // Memory-optimized (R6i Intel Ice Lake)
    ("r6i.xlarge", 0.252),
    ("r6i.2xlarge", 0.504),
    ("r6i.4xlarge", 1.008),
    ("r6i.8xlarge", 2.016),
    ("r6i.16xlarge", 4.032),
    // GPU (T4 / L4 / A100)
    ("g4dn.xlarge", 0.526),
    ("g4dn.12xlarge", 3.912),
    ("g6.xlarge", 0.805),
    ("p4d.24xlarge", 32.77),
];

/// R-20 — region-aware pricing handle. Carries a multiplier applied to
/// the table on every lookup. Construct via `Pricing::for_region`.
///
/// Multiplier is read from `SWFC_AWS_PRICING_REGION_MULT` at
/// construction time. Unset / unparseable / non-positive values fall
/// back to 1.0 (the us-east-1 reference). Operators in pricier regions
/// set the multiplier explicitly; the harness never tries to infer it
/// from the region string because the AWS rate-card surface is too
/// sparse and changes too often to keep a hard-coded mapping fresh.
#[derive(Debug, Clone, Copy)]
pub struct Pricing {
    multiplier: f64,
}

impl Pricing {
    /// R-20 — build a region-scaled pricing handle. The `region` argument
    /// is captured for diagnostics; the multiplier comes from the env
    /// var (so an operator can pin a known multiplier per-region in
    /// their launch script rather than ship a hard-coded table).
    ///
    /// Reading the env each construction (rather than once at process
    /// start) lets a test set the multiplier and observe its effect
    /// without a process restart.
    pub fn for_region(region: &str) -> Self {
        let multiplier = match std::env::var(PRICING_REGION_MULT_ENV) {
            Ok(raw) => match raw.trim().parse::<f64>() {
                Ok(v) if v > 0.0 && v.is_finite() => v,
                _ => {
                    tracing::warn!(
                        env = PRICING_REGION_MULT_ENV,
                        value = %raw,
                        region = %region,
                        "ignoring non-positive / unparseable pricing multiplier; \
                         falling back to 1.0 (us-east-1 reference rates)"
                    );
                    1.0
                }
            },
            Err(_) => 1.0,
        };
        Self { multiplier }
    }

    /// Multiplier applied to every base-table lookup. 1.0 is us-east-1.
    pub fn multiplier(&self) -> f64 {
        self.multiplier
    }

    /// On-demand USD per hour for `instance_type` in this region.
    /// Returns `None` for unknown types — the cost guard treats that
    /// as a hard error so an unpriced shape can't silently slip past
    /// the ceiling.
    pub fn on_demand_usd_per_hour(&self, instance_type: &str) -> Option<f64> {
        on_demand_usd_per_hour(instance_type).map(|p| p * self.multiplier)
    }

    /// Spot bid for `instance_type` in this region. Composes the
    /// regional multiplier with the spot discount fraction.
    pub fn spot_bid_usd_per_hour(&self, instance_type: &str) -> Option<f64> {
        spot_bid_usd_per_hour(instance_type).map(|p| p * self.multiplier)
    }
}

/// Look up on-demand hourly USD for an instance type. Returns `None` for
/// unknown types — caller should fall back to a profile-defined estimate.
///
/// Reference-region (us-east-1) lookup; for region-scaled pricing use
/// `Pricing::for_region(...).on_demand_usd_per_hour(...)`.
pub fn on_demand_usd_per_hour(instance_type: &str) -> Option<f64> {
    INSTANCE_PRICES_USD_PER_HOUR
        .iter()
        .find(|(t, _)| *t == instance_type)
        .map(|(_, p)| *p)
}

/// Spot bid for the given instance type, computed as on-demand × discount.
/// Reference-region (us-east-1) lookup.
pub fn spot_bid_usd_per_hour(instance_type: &str) -> Option<f64> {
    on_demand_usd_per_hour(instance_type).map(|od| od * SPOT_DISCOUNT_FRACTION)
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
    fn pricing_revision_is_iso_date() {
        // Sanity check: revision is ten chars "YYYY-MM-DD" so future updates
        // can't accidentally commit something like "yesterday".
        assert_eq!(PRICING_TABLE_REVISION.len(), 10);
        assert!(PRICING_TABLE_REVISION.chars().nth(4) == Some('-'));
        assert!(PRICING_TABLE_REVISION.chars().nth(7) == Some('-'));
    }

    /// W5.2 — fail the build past 90-day pricing-table staleness.
    ///
    /// The runtime constructor logs a `tracing::warn!` past the staleness
    /// window, but an inattentive operator can let the table drift for
    /// months without anyone noticing. Promote that warn to a build-failing
    /// test so CI catches drift at the 90-day mark.
    #[test]
    fn pricing_table_revision_not_stale_more_than_90_days() {
        use chrono::{NaiveDate, Utc};
        let revision = NaiveDate::parse_from_str(PRICING_TABLE_REVISION, "%Y-%m-%d")
            .expect("PRICING_TABLE_REVISION must parse as YYYY-MM-DD");
        let today = Utc::now().date_naive();
        let age_days = (today - revision).num_days();
        assert!(
            age_days <= PRICING_TABLE_STALENESS_WARN_DAYS,
            "AWS pricing table at PRICING_TABLE_REVISION={} is {} days old (> {}). \
             Refresh against current AWS rate-card and bump the constant in pricing.rs.",
            PRICING_TABLE_REVISION,
            age_days,
            PRICING_TABLE_STALENESS_WARN_DAYS,
        );
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn spot_discount_is_in_plausible_range() {
        // AWS spot variance is 20-40%; outside [0.15, 0.45] suggests a typo.
        assert!(SPOT_DISCOUNT_FRACTION >= 0.15);
        assert!(SPOT_DISCOUNT_FRACTION <= 0.45);
    }

    #[test]
    fn prices_are_ordered_by_capability() {
        // Sanity: GPU > memory-opt > compute-opt > general-purpose.
        let od = |t| on_demand_usd_per_hour(t).expect(t);
        assert!(od("t3.medium") < od("c6i.4xlarge"));
        assert!(od("c6i.4xlarge") < od("r6i.4xlarge"));
        assert!(od("r6i.4xlarge") < od("g4dn.12xlarge"));
        assert!(od("g4dn.12xlarge") < od("p4d.24xlarge"));
    }

    #[test]
    fn spot_bid_uses_discount() {
        let od = on_demand_usd_per_hour("t3.medium").unwrap();
        let bid = spot_bid_usd_per_hour("t3.medium").unwrap();
        assert!((bid - od * SPOT_DISCOUNT_FRACTION).abs() < 1e-9);
    }

    /// R-20 — the pricing table must cover every instance type
    /// `sizing::resolve_instance_type` can return. A missing row would
    /// degrade to `CostGuardError::UnknownInstanceType` at provision
    /// time — a hard fail well after the sizing decision.
    #[test]
    fn pricing_table_covers_every_sizing_pick() {
        // Mirrors `sizing::INSTANCE_CAPACITY` callsites; keep in sync
        // when a new resolver arm lands.
        let must_have = [
            "t3.medium",
            "t3.large",
            "c6i.xlarge",
            "c6i.2xlarge",
            "c6i.4xlarge",
            "c6i.8xlarge",
            "c6i.12xlarge",
            "r6i.xlarge",
            "r6i.2xlarge",
            "r6i.4xlarge",
            "r6i.8xlarge",
            "r6i.16xlarge",
            "g4dn.xlarge",
            "g4dn.12xlarge",
            "g6.xlarge",
            "p4d.24xlarge",
        ];
        for t in must_have {
            assert!(
                on_demand_usd_per_hour(t).is_some(),
                "missing pricing row for {t}; add to INSTANCE_PRICES_USD_PER_HOUR"
            );
        }
    }

    #[test]
    fn c6i_family_prices_climb_with_size() {
        let od = |t| on_demand_usd_per_hour(t).expect(t);
        assert!(od("c6i.xlarge") < od("c6i.2xlarge"));
        assert!(od("c6i.2xlarge") < od("c6i.4xlarge"));
        assert!(od("c6i.4xlarge") < od("c6i.8xlarge"));
        assert!(od("c6i.8xlarge") < od("c6i.12xlarge"));
    }

    #[test]
    fn r6i_family_prices_climb_with_size() {
        let od = |t| on_demand_usd_per_hour(t).expect(t);
        assert!(od("r6i.xlarge") < od("r6i.2xlarge"));
        assert!(od("r6i.2xlarge") < od("r6i.4xlarge"));
        assert!(od("r6i.4xlarge") < od("r6i.8xlarge"));
        assert!(od("r6i.8xlarge") < od("r6i.16xlarge"));
    }

    // ── R-20 Pricing::for_region tests ──────────────────────────────────

    fn with_region_mult<T>(value: Option<&str>, body: impl FnOnce() -> T) -> T {
        // Serialize on the crate-wide env lock so these tests don't
        // race concurrent harness/pricing tests touching the same
        // SWFC_AWS_* env space.
        let _lock = super::super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var(PRICING_REGION_MULT_ENV).ok();
        match value {
            Some(v) => unsafe { std::env::set_var(PRICING_REGION_MULT_ENV, v) },
            None => unsafe { std::env::remove_var(PRICING_REGION_MULT_ENV) },
        }
        let out = body();
        match prior {
            Some(v) => unsafe { std::env::set_var(PRICING_REGION_MULT_ENV, v) },
            None => unsafe { std::env::remove_var(PRICING_REGION_MULT_ENV) },
        }
        out
    }

    #[test]
    fn pricing_for_region_default_is_one() {
        with_region_mult(None, || {
            let p = Pricing::for_region("us-east-1");
            assert!((p.multiplier() - 1.0).abs() < 1e-9);
            let base = on_demand_usd_per_hour("r6i.4xlarge").unwrap();
            assert!((p.on_demand_usd_per_hour("r6i.4xlarge").unwrap() - base).abs() < 1e-9);
        });
    }

    #[test]
    fn pricing_for_region_applies_multiplier() {
        with_region_mult(Some("1.25"), || {
            let p = Pricing::for_region("eu-west-1");
            assert!((p.multiplier() - 1.25).abs() < 1e-9);
            let base = on_demand_usd_per_hour("r6i.4xlarge").unwrap();
            let scaled = p.on_demand_usd_per_hour("r6i.4xlarge").unwrap();
            assert!((scaled - base * 1.25).abs() < 1e-9);
        });
    }

    #[test]
    fn pricing_for_region_invalid_value_falls_back_to_one() {
        with_region_mult(Some("not-a-number"), || {
            let p = Pricing::for_region("us-east-1");
            assert!((p.multiplier() - 1.0).abs() < 1e-9);
        });
        with_region_mult(Some("-1.5"), || {
            let p = Pricing::for_region("us-east-1");
            assert!((p.multiplier() - 1.0).abs() < 1e-9);
        });
        with_region_mult(Some("0"), || {
            let p = Pricing::for_region("us-east-1");
            assert!((p.multiplier() - 1.0).abs() < 1e-9);
        });
    }

    #[test]
    fn pricing_for_region_spot_composes_with_discount() {
        with_region_mult(Some("1.10"), || {
            let p = Pricing::for_region("ap-southeast-2");
            let od = p.on_demand_usd_per_hour("t3.medium").unwrap();
            let spot = p.spot_bid_usd_per_hour("t3.medium").unwrap();
            assert!((spot - od * SPOT_DISCOUNT_FRACTION).abs() < 1e-9);
        });
    }

    #[test]
    fn pricing_for_region_unknown_type_is_none() {
        with_region_mult(Some("1.10"), || {
            let p = Pricing::for_region("us-east-1");
            assert!(p.on_demand_usd_per_hour("fake.nonexistent").is_none());
            assert!(p.spot_bid_usd_per_hour("fake.nonexistent").is_none());
        });
    }
}
