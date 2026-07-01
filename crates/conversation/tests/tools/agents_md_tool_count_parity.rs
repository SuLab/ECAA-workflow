//! Catches drift in documentation of Tool::COUNT. The audit
//! verified Tool::COUNT = 23 across BatchableTool (15 variants) and
//! HighImpactTool (8 variants).

use ecaa_workflow_conversation::Tool;

#[test]
fn tool_count_is_23() {
    // 15 BatchableTool variants: ClassifyIntake, GetTaxonomyInfo, GetSessionState,
    // GetClassificationEvidence, GetTaskResult, GetLiteratureContext, ListAtoms,
    // SetIntakeField, SetIntakeMethod, AppendIntakeProse, SetIntakeExcludedAtoms,
    // SetIntakeModality, ProposeSummaryConfirmation, ProposeQuickReplies,
    // ProbeDataset
    //
    // 8 HighImpactTool variants: AmendStageMethod, SelectSensitivityWinner,
    // RerunTask, BranchSession, EmitPackage, StartExecution,
    // ProposeHypothesizedNode, ProposeHypothesizedRenderer
    //
    // Total: 15 + 8 = 23
    assert_eq!(
        Tool::COUNT,
        23,
        "Tool::COUNT drifted from 23. Update CLAUDE.md and AGENTS.md \
         documentation, plus this test, in the same PR as any tool addition.",
    );
}
