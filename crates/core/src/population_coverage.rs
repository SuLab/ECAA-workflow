//! Population-coverage gating (v3 §11.X).
//!
//! A `PopulationCoverageStatement` is the workflow-level, config-authored
//! statement of which sample cohorts the workflow has been **validated
//! on**. The composer consults it at planning time when (1) an active
//! `PolicyBundle` declares the session clinical and (2) the
//! `WorkflowIntent.sample_cohort` is set. Mismatch produces a typed
//! `Refusal { kind: PopulationOutOfCoverage {... } }` outcome instead
//! of a silent "we'll just run it anyway" composition.
//!
//! # Framing constraint
//!
//! The gate runs against the **workflow's** coverage statement — it
//! asks "has this workflow been validated on a cohort matching the
//! one the SME provided?". The sample's population descriptor is
//! treated as **metadata about workflow applicability**, not as an
//! access-control predicate on the user's identity. The system never
//! refuses an SME because of who *they* are; it refuses a composition
//! when the workflow's validation envelope doesn't cover the data the
//! SME wants to analyze. The waiver mechanism (see [`PopulationWaiver`])
//! is the explicit escape hatch the SME's clinical lead can sign so a
//! pediatric solid-tumor cohort can be processed by an adult-validated
//! workflow with the loss-of-validation declared and recorded.
//!
//! # YAML on disk
//!
//! One file per workflow id at
//! `config/population-coverage/<workflow_id>.yaml`. Schema sidecar:
//! `config/population-coverage/_population-coverage.schema.json`.
//! Starter file for the rnaseq-de-clinical archetype lives at
//! `config/population-coverage/rnaseq-de-clinical.yaml`.
//!
//! # LLM extraction (deferred)
//!
//! v3 §11.X explicitly defers LLM-extraction of coverage statements
//! from validation-paper citations. Currently only config-authored
//! statements are supported; future work may add an extractor that proposes a
//! coverage statement for SME review, but the gate's source of truth
//! is always the on-disk YAML.

use crate::workflow_contracts::policy_rule_id::PolicyRuleId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Workflow-level, config-authored statement of which sample cohorts
/// the workflow was validated on.
///
/// Loaded from `config/population-coverage/<workflow_id>.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PopulationCoverageStatement {
    /// Workflow (archetype) id this statement applies to.
    pub workflow_id: String,
    /// Validation cohorts the workflow was evaluated on.
    pub validated_cohorts: Vec<CohortDescriptor>,
    /// Populations explicitly outside validation scope. Surfaces in
    /// the UI as "the workflow was tested but NOT validated for these
    /// cohorts" so the SME can see explicit gaps, not just an absence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explicitly_untested: Vec<CohortDescriptor>,
    /// Cite sources for the validation claim (DOIs or PMIDs). At least
    /// one is recommended for clinical-grade workflows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<String>,
}

/// Structured cohort descriptor. Both the workflow's validated cohorts
/// and the SME's sample-cohort claim use this type so `covers()` can
/// match them by structural fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct CohortDescriptor {
    /// Free-text label (e.g. "GIAB Ashkenazi trio", "1000 Genomes EUR").
    pub label: String,
    /// Optional structured population code (NIH MOR taxonomy, OMOP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub population_code: Option<String>,
    /// Age range covered (e.g. "adult", "pediatric", "all").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub age_band: Option<String>,
    /// Sample type (e.g. "solid_tumor", "liquid_biopsy", "germline").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sample_type: Option<String>,
    /// Sample size for the cohort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub n: Option<usize>,
}

/// One signed waiver authorizing a workflow to process an out-of-coverage
/// cohort. Recorded on the active `PolicyBundle` and durably emitted to
/// `runtime/decisions.jsonl` for provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PopulationWaiver {
    /// Workflow id the waiver applies to (matches `PopulationCoverageStatement.workflow_id`).
    pub workflow_id: String,
    /// Free-text label of the role that signed (e.g. "clinical_lead",
    /// "irb_chair"). The server resolves this against the user's
    /// role at waiver-POST time; the field here is the recorded
    /// post-resolution name.
    pub waiving_authority: String,
    /// Free-text rationale. Required — empty rationale is rejected by
    /// the POST endpoint.
    pub rationale: String,
    /// ISO-8601 timestamp the waiver was recorded. Stable string form
    /// so the byte-reproducibility contract isn't broken by re-emission.
    pub waived_at: String,
    /// Stable id of the policy rule the waiver overrides. Matches the
    /// `RefusalReport.id` of the refusal that triggered the waiver
    /// request so the audit trail is bidirectional. Registry-validated
    /// via [`PolicyRuleId::new`] when the waiver
    /// is minted at the server; deserialization from on-disk records
    /// is permissive (legacy ids round-trip via
    /// [`PolicyRuleId::unchecked`]).
    pub policy_rule_id: PolicyRuleId,
}

/// Errors returned by [`PopulationCoverageStatement::load_from_path`]
/// and the file resolver.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum PopulationCoverageError {
    /// The workflow id has no registered coverage statement on disk.
    /// Caller should fall through (no statement = no gate) rather than
    /// fail the whole composition.
    #[error("workflow {0} has no registered coverage statement")]
    NoStatement(String),
    /// The sample cohort failed the `covers()` check and no in-scope
    /// waiver was found. The planner converts this to a
    /// `RefusalKind::PopulationOutOfCoverage` outcome.
    #[error(
        "sample population {sample} is outside validated cohorts {validated:?} \
         for workflow {workflow_id}; waiver required"
    )]
    OutOfCoverage {
        /// Workflow id.
        workflow_id: String,
        /// Sample.
        sample: String,
        /// Validated.
        validated: Vec<String>,
    },
    /// Filesystem error while loading the statement.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// YAML parse error.
    #[error("yaml parse: {0}")]
    YamlParse(#[from] serde_yml::Error),
}

impl PopulationCoverageStatement {
    /// Load a single coverage statement from a YAML file. Returns
    /// `NoStatement` when the file doesn't exist so the composer can
    /// distinguish "no coverage gate applies" from "load failed."
    pub fn load_from_path(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, PopulationCoverageError> {
        let p = path.as_ref();
        if !p.exists() {
            // Try to recover a workflow id for the error from the file stem.
            let id = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("<unknown>")
                .to_string();
            return Err(PopulationCoverageError::NoStatement(id));
        }
        let yaml = std::fs::read_to_string(p)?;
        let stmt: PopulationCoverageStatement = serde_yml::from_str(&yaml)?;
        Ok(stmt)
    }

    /// True iff the workflow's validated cohorts include a match for
    /// the SME-provided sample cohort.
    ///
    /// Matching is intentionally inclusive: label exact-match OR
    /// `population_code` exact-match OR (`age_band` + `sample_type`
    /// joint match when both sides specify them). Use the structured
    /// fields when you have them — they let the workflow declare
    /// validation by demographic envelope, not by hardcoded label
    /// list.
    pub fn covers(&self, sample: &CohortDescriptor) -> bool {
        self.validated_cohorts
            .iter()
            .any(|c| descriptor_matches(c, sample))
    }
}

/// Cohort-match relation. Symmetric, conservative:
///
/// 1. `label` exact-match, OR
/// 2. `population_code` exact-match (both sides must specify), OR
/// 3. `age_band` + `sample_type` joint match (both fields specified on
///    both sides, all four equal).
///
/// `n` is informational only and never affects the match.
fn descriptor_matches(a: &CohortDescriptor, b: &CohortDescriptor) -> bool {
    if a.label == b.label {
        return true;
    }
    if a.population_code.is_some() && a.population_code == b.population_code {
        return true;
    }
    if a.age_band.is_some()
        && b.age_band.is_some()
        && a.sample_type.is_some()
        && b.sample_type.is_some()
        && a.age_band == b.age_band
        && a.sample_type == b.sample_type
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cohort(label: &str) -> CohortDescriptor {
        CohortDescriptor {
            label: label.into(),
            population_code: None,
            age_band: None,
            sample_type: None,
            n: None,
        }
    }

    #[test]
    fn covers_returns_true_on_label_match() {
        let stmt = PopulationCoverageStatement {
            workflow_id: "rnaseq-de-clinical".into(),
            validated_cohorts: vec![cohort("GTEx v8 normal tissue")],
            explicitly_untested: vec![],
            citations: vec![],
        };
        assert!(stmt.covers(&cohort("GTEx v8 normal tissue")));
    }

    #[test]
    fn covers_returns_false_on_label_mismatch() {
        let stmt = PopulationCoverageStatement {
            workflow_id: "rnaseq-de-clinical".into(),
            validated_cohorts: vec![cohort("GTEx v8 normal tissue")],
            explicitly_untested: vec![],
            citations: vec![],
        };
        assert!(!stmt.covers(&cohort("Pediatric solid tumor")));
    }

    #[test]
    fn covers_returns_true_on_population_code_match() {
        let validated = CohortDescriptor {
            label: "TCGA pan-cancer".into(),
            population_code: Some("tcga_panchort_v15".into()),
            age_band: Some("adult".into()),
            sample_type: Some("solid_tumor".into()),
            n: Some(11_315),
        };
        let sample = CohortDescriptor {
            label: "Different label".into(),
            population_code: Some("tcga_panchort_v15".into()),
            age_band: None,
            sample_type: None,
            n: None,
        };
        let stmt = PopulationCoverageStatement {
            workflow_id: "rnaseq-de-clinical".into(),
            validated_cohorts: vec![validated],
            explicitly_untested: vec![],
            citations: vec![],
        };
        assert!(stmt.covers(&sample));
    }

    #[test]
    fn covers_returns_true_on_age_band_plus_sample_type_match() {
        let validated = CohortDescriptor {
            label: "Validated set A".into(),
            population_code: None,
            age_band: Some("adult".into()),
            sample_type: Some("solid_tumor".into()),
            n: None,
        };
        let sample = CohortDescriptor {
            label: "Unrelated label".into(),
            population_code: None,
            age_band: Some("adult".into()),
            sample_type: Some("solid_tumor".into()),
            n: None,
        };
        let stmt = PopulationCoverageStatement {
            workflow_id: "rnaseq-de-clinical".into(),
            validated_cohorts: vec![validated],
            explicitly_untested: vec![],
            citations: vec![],
        };
        assert!(stmt.covers(&sample));
    }

    #[test]
    fn covers_age_band_alone_is_not_a_match() {
        // age_band match without sample_type match should NOT match.
        let validated = CohortDescriptor {
            label: "A".into(),
            population_code: None,
            age_band: Some("adult".into()),
            sample_type: Some("solid_tumor".into()),
            n: None,
        };
        let sample = CohortDescriptor {
            label: "B".into(),
            population_code: None,
            age_band: Some("adult".into()),
            sample_type: Some("liquid_biopsy".into()),
            n: None,
        };
        let stmt = PopulationCoverageStatement {
            workflow_id: "rnaseq-de-clinical".into(),
            validated_cohorts: vec![validated],
            explicitly_untested: vec![],
            citations: vec![],
        };
        assert!(!stmt.covers(&sample));
    }

    #[test]
    fn round_trip_serde() {
        let stmt = PopulationCoverageStatement {
            workflow_id: "rnaseq-de-clinical".into(),
            validated_cohorts: vec![CohortDescriptor {
                label: "GTEx v8 normal tissue".into(),
                population_code: Some("gtex_v8_eur".into()),
                age_band: Some("adult".into()),
                sample_type: Some("bulk_tissue".into()),
                n: Some(17_382),
            }],
            explicitly_untested: vec![CohortDescriptor {
                label: "Pediatric solid tumor".into(),
                population_code: None,
                age_band: Some("pediatric".into()),
                sample_type: Some("solid_tumor".into()),
                n: None,
            }],
            citations: vec!["doi:10.1038/s41467-019-12873-4".into()],
        };
        let json = serde_json::to_string(&stmt).unwrap();
        let back: PopulationCoverageStatement = serde_json::from_str(&json).unwrap();
        assert_eq!(stmt, back);
    }

    #[test]
    fn load_from_missing_path_returns_no_statement_error() {
        let path = std::path::Path::new("/tmp/no-such-file-population-coverage.yaml");
        let r = PopulationCoverageStatement::load_from_path(path);
        assert!(matches!(r, Err(PopulationCoverageError::NoStatement(_))));
    }

    #[test]
    fn population_waiver_round_trips() {
        let w = PopulationWaiver {
            workflow_id: "rnaseq-de-clinical".into(),
            waiving_authority: "clinical_lead".into(),
            rationale: "Pediatric IRB acknowledged off-label use".into(),
            waived_at: "2026-05-11T12:00:00Z".into(),
            policy_rule_id: PolicyRuleId::unchecked("population_coverage:rnaseq-de-clinical"),
        };
        let json = serde_json::to_string(&w).unwrap();
        let back: PopulationWaiver = serde_json::from_str(&json).unwrap();
        assert_eq!(w, back);
    }
}
