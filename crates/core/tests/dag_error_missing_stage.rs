//! Integration coverage for `DagError::MissingStage`.
//!
//! Today the variant is declared in `DagError` (plan §S5.12 typed-error
//! coverage) but has no construction site in the validator — it is reserved
//! for the upcoming builder pathway where a `Task` references a stage id
//! that the taxonomy doesn't define. The test below pins the shape so
//! consumers can pattern-match without breaking on a future refactor; the
//! variant's Display contract is also locked in so the blocker UI keeps
//! rendering the same operator-visible string.

use ecaa_workflow_core::dag::DagError;

#[test]
fn missing_stage_variant_matches_with_stage_id_payload() {
    let err = DagError::MissingStage {
        stage_id: "discover_alignment".into(),
    };

    match &err {
        DagError::MissingStage { stage_id } => {
            assert_eq!(stage_id, "discover_alignment");
        }
        other => panic!("expected MissingStage, got {:?}", other),
    }

    // Lock in the Display contract so the blocker UI doesn't silently
    // drift if the wording is reshuffled.
    let rendered = format!("{err}");
    assert!(
        rendered.contains("discover_alignment"),
        "Display output must surface the stage id: {rendered}",
    );
    assert!(
        rendered.contains("missing"),
        "Display output must declare the stage is missing: {rendered}",
    );
}
