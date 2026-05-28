//!
//! After four consecutive `IntakeFollowup` turns on the same intake, the
//! system prompt for the next turn must carry a convergence nudge
//! directing the LLM toward `propose_summary_confirmation` rather than
//! letting it spin asking more clarifying questions.
//!
//! Today the LLM stays in `IntakeFollowup` indefinitely (variant-calling,
//! single-cell, edge-case-multiomics, paper-07, paper-10 all hit this).
//! The nudge is a text-only, server-side prompt augmentation — the
//! confirmation/emit gates are still deterministic, so this is a UX
//! convergence signal, not an LLM bypass.
//!
//! The streak is per-turn (not per-transition): `tool_loop::run_tool_loop`
//! calls `Session::note_turn_end_intake_followup` exactly once at every
//! successful exit path. That bump-or-reset based on the turn's final
//! state matches the plan's "4 consecutive followup turns" wording.

use ecaa_workflow_conversation::{build_system_prompt, Session, SessionState};

/// `Session::intake_followup_streak` starts at 0 on a fresh session.
#[test]
fn fresh_session_streak_is_zero() {
    let s = Session::new(false);
    assert_eq!(s.intake_followup_streak, 0);
}

/// Per-turn observer bumps the streak when the turn ended in
/// `IntakeFollowup`, regardless of whether the session was already in
/// that state at turn start.
#[test]
fn note_turn_end_bumps_when_ending_in_intake_followup() {
    let mut s = Session::new(false);
    s.state = SessionState::IntakeFollowup;

    s.note_turn_end_intake_followup();
    assert_eq!(s.intake_followup_streak, 1);

    s.note_turn_end_intake_followup();
    s.note_turn_end_intake_followup();
    s.note_turn_end_intake_followup();
    assert_eq!(s.intake_followup_streak, 4);
}

/// Per-turn observer resets the streak when the turn ended in any other
/// state. Mirrors "turn out of the followup loop" — `PendingConfirmation`,
/// `Intake`, `Emitted`, `Blocked`, etc.
#[test]
fn note_turn_end_resets_when_ending_outside_intake_followup() {
    let mut s = Session::new(false);
    s.state = SessionState::IntakeFollowup;
    s.intake_followup_streak = 6;

    s.state = SessionState::PendingConfirmation { stage: None };
    s.note_turn_end_intake_followup();
    assert_eq!(s.intake_followup_streak, 0);

    // and from Intake too
    s.state = SessionState::IntakeFollowup;
    s.intake_followup_streak = 6;
    s.state = SessionState::Intake;
    s.note_turn_end_intake_followup();
    assert_eq!(s.intake_followup_streak, 0);
}

/// `saturating_add` guards against u32 overflow on a runaway session.
/// Documented invariant — a pathological session that never leaves
/// `IntakeFollowup` should still produce a well-defined prompt.
#[test]
fn note_turn_end_saturates_on_overflow() {
    let mut s = Session::new(false);
    s.state = SessionState::IntakeFollowup;
    s.intake_followup_streak = u32::MAX;
    s.note_turn_end_intake_followup();
    assert_eq!(s.intake_followup_streak, u32::MAX);
}

/// The convergence nudge is absent from the system prompt while the
/// streak is below the threshold (4).
#[test]
fn nudge_absent_below_threshold() {
    let mut s = Session::new(false);
    s.state = SessionState::IntakeFollowup;
    s.intake_followup_streak = 3;
    let blocks = build_system_prompt(&s);
    let joined = blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !joined.contains("CONVERGENCE NUDGE"),
        "nudge must NOT fire at streak=3 (threshold is 4); got prompt:\n{}",
        joined
    );
}

/// After 4 consecutive `IntakeFollowup` turns the system prompt for the
/// next turn carries the convergence nudge. Asserts both the section
/// header and the directive phrasing the LLM is meant to act on.
#[test]
fn fifth_intake_followup_turn_includes_convergence_nudge() {
    let mut s = Session::new(false);
    s.state = SessionState::IntakeFollowup;
    s.intake_followup_streak = 4;

    let blocks = build_system_prompt(&s);
    let joined = blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        joined.contains("CONVERGENCE NUDGE"),
        "5th IntakeFollowup turn must carry the convergence-nudge section header; got:\n{}",
        joined
    );
    assert!(
        joined.contains("you have enough detail")
            || joined.contains("call propose_summary_confirmation"),
        "convergence nudge must direct the LLM toward propose_summary_confirmation; got:\n{}",
        joined
    );
}

/// The nudge block lives in the UNCACHED suffix so toggling it on/off
/// across turns doesn't invalidate the cacheable prefix (role + class +
/// taxonomy). Mirrors the soft-landing / escalation discipline in
/// `tool_loop.rs`.
#[test]
fn convergence_nudge_block_is_uncached() {
    let mut s = Session::new(false);
    s.state = SessionState::IntakeFollowup;
    s.intake_followup_streak = 6;

    let blocks = build_system_prompt(&s);
    let nudge_block = blocks
        .iter()
        .find(|b| b.text.contains("CONVERGENCE NUDGE"))
        .expect("nudge block must be present at streak >= 4");
    assert!(
        !nudge_block.cache,
        "convergence-nudge block must not carry cache_control: ephemeral",
    );
}
