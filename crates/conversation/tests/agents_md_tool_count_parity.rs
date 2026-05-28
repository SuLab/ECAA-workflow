//! Catches drift in documentation of Tool::COUNT. The audit
//! verified Tool::COUNT = 22 across BatchableTool (14 variants) and
//! HighImpactTool (8 variants).

use scripps_workflow_conversation::Tool;

#[test]
fn tool_count_is_22() {
    // 14 BatchableTool variants: ClassifyIntake, GetTaxonomyInfo, GetSessionState,
    // GetClassificationEvidence, GetTaskResult, GetLiteratureContext, ListAtoms,
    // SetIntakeField, SetIntakeMethod, AppendIntakeProse, SetIntakeExcludedAtoms,
    // SetIntakeModality, ProposeSummaryConfirmation, ProposeQuickReplies
    //
    // 8 HighImpactTool variants: AmendStageMethod, SelectSensitivityWinner,
    // RerunTask, BranchSession, EmitPackage, StartExecution,
    // ProposeHypothesizedNode, ProposeHypothesizedRenderer
    //
    // Total: 14 + 8 = 22
    assert_eq!(
        Tool::COUNT,
        22,
        "Tool::COUNT drifted from 22. Update CLAUDE.md and AGENTS.md \
         documentation, plus this test, in the same PR as any tool addition.",
    );
}
