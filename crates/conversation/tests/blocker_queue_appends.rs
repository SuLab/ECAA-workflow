//! Regression guard: `SessionState::Blocked` keeps a queue of
//! blocker entries so concurrent blockers from different tasks are not
//! overwritten by the latest event.

use scripps_workflow_conversation::session::{Session, SessionState, StateTrigger};
use scripps_workflow_core::blocker::BlockerKind;

fn seed_emitted_session() -> Session {
    let mut s = Session::new(false);
    s.try_transition(StateTrigger::AppendProse).unwrap();
    s.try_transition(StateTrigger::ProposeSummaryConfirmation)
        .unwrap();
    s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
    s.try_transition(StateTrigger::EmitPackageStart).unwrap();
    s.try_transition(StateTrigger::EmitPackageOk).unwrap();
    assert_eq!(
        s.state,
        SessionState::Emitted,
        "fixture precondition: session must reach Emitted before blocker append tests"
    );
    s.emitted_package_path = Some(std::path::PathBuf::from("/tmp/pkg-test"));
    s
}

#[test]
fn second_blocker_appends_instead_of_silently_dropping() {
    let mut s = seed_emitted_session();

    s.try_transition(StateTrigger::HarnessTaskBlocked {
        task_id: "task-a".into(),
        detail: "bad cols".into(),
        blocker_kind: BlockerKind::DataShapeMismatch {
            expected: "matrix".into(),
            actual: "list".into(),
        },
    })
    .unwrap();

    s.try_transition(StateTrigger::HarnessTaskBlocked {
        task_id: "task-b".into(),
        detail: "agent died".into(),
        blocker_kind: BlockerKind::AgentError {
            message: "agent died".into(),
        },
    })
    .unwrap();

    match &s.state {
        SessionState::Blocked { blockers, .. } => {
            assert_eq!(
                blockers.len(),
                2,
                "second concurrent blocker must append, not drop the first"
            );
            assert_eq!(blockers[0].task_id, "task-a");
            assert_eq!(blockers[1].task_id, "task-b");
            assert!(matches!(
                blockers[0].kind,
                BlockerKind::DataShapeMismatch { .. }
            ));
            assert!(matches!(blockers[1].kind, BlockerKind::AgentError { .. }));
            assert_ne!(blockers[0].blocker_id, blockers[1].blocker_id);
        }
        other => panic!("expected Blocked with two entries, got {:?}", other),
    }
}

#[test]
fn duplicate_task_id_refreshes_in_place_not_appended() {
    let mut s = seed_emitted_session();

    s.try_transition(StateTrigger::HarnessTaskBlocked {
        task_id: "task-a".into(),
        detail: "first attempt".into(),
        blocker_kind: BlockerKind::DataShapeMismatch {
            expected: "matrix".into(),
            actual: "list".into(),
        },
    })
    .unwrap();

    s.try_transition(StateTrigger::HarnessTaskBlocked {
        task_id: "task-a".into(),
        detail: "second attempt".into(),
        blocker_kind: BlockerKind::AgentError {
            message: "agent died".into(),
        },
    })
    .unwrap();

    match &s.state {
        SessionState::Blocked { blockers, .. } => {
            assert_eq!(
                blockers.len(),
                1,
                "duplicate task_id must refresh in place, not add a second entry"
            );
            assert_eq!(blockers[0].task_id, "task-a");
            assert_eq!(blockers[0].message, "Task task-a blocked: second attempt");
            assert!(matches!(blockers[0].kind, BlockerKind::AgentError { .. }));
        }
        other => panic!("expected Blocked with one entry, got {:?}", other),
    }
}
