//! Per-plan §8.B.1: analysis-type plugin primitive. Closed enum by
//! design (D1) — adding a new variant requires a workspace PR plus a
//! matching taxonomy, prompt block, overlay policy, and (for non-bio
//! classes per D5) a container.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Hash,
    TS,
    Default,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// ProjectClass discriminant.
pub enum ProjectClass {
    #[default]
    /// Bioinformatics variant.
    Bioinformatics,
    /// ClinicalTrial variant.
    ClinicalTrial,
    /// TimeSeriesForecast variant.
    TimeSeriesForecast,
}

impl ProjectClass {
    /// Stable string used on the wire and in keyword-routing configs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bioinformatics => "bioinformatics",
            Self::ClinicalTrial => "clinical_trial",
            Self::TimeSeriesForecast => "time_series_forecast",
        }
    }

    /// Non-bioinformatics classes must declare a `preferred_container`
    /// in their taxonomy (D5). Bio is grandfathered — it may opt in but
    /// is not required to.
    pub fn requires_container(&self) -> bool {
        !matches!(self, Self::Bioinformatics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_bioinformatics() {
        assert_eq!(ProjectClass::default(), ProjectClass::Bioinformatics);
    }

    #[test]
    fn serde_round_trip_snake_case() {
        for (class, wire) in [
            (ProjectClass::Bioinformatics, "\"bioinformatics\""),
            (ProjectClass::ClinicalTrial, "\"clinical_trial\""),
            (ProjectClass::TimeSeriesForecast, "\"time_series_forecast\""),
        ] {
            let json = serde_json::to_string(&class).unwrap();
            assert_eq!(json, wire);
            let back: ProjectClass = serde_json::from_str(&json).unwrap();
            assert_eq!(class, back);
        }
    }

    #[test]
    fn requires_container_reflects_d5() {
        assert!(!ProjectClass::Bioinformatics.requires_container());
        assert!(ProjectClass::ClinicalTrial.requires_container());
        assert!(ProjectClass::TimeSeriesForecast.requires_container());
    }

    #[test]
    fn as_str_matches_wire_format() {
        assert_eq!(ProjectClass::Bioinformatics.as_str(), "bioinformatics");
        assert_eq!(ProjectClass::ClinicalTrial.as_str(), "clinical_trial");
        assert_eq!(
            ProjectClass::TimeSeriesForecast.as_str(),
            "time_series_forecast"
        );
    }
}
