//! `ModelRoutingTable` and predicate DSL.
//!
//! Replaces the three imperative `if`s + literal `0.3` threshold +
//! hard-pinned side-call models in `model_policy::ModelPolicy` with a
//! YAML-driven rule table. Adding a new routing rule is a YAML row
//! plus (only when introducing a new predicate kind) a small parser
//! addition — no Rust method per side-call.
//!
//! Default seed lives at `config/model-policy.yaml`. The seed is
//! `include_str!`-embedded so the registry never depends on
//! filesystem state at runtime; an operator can still override via
//! `ECAA_MODEL_POLICY_PATH` for ad-hoc experimentation.
//!
//! Behavior parity with the pre-refactor imperative path is asserted
//! by the existing pin tests in `super::tests` (careful-mode, blocked
//! one-shot, low-confidence, default Sonnet) plus new round-trip
//! tests that load the seed and assert each rule.

use crate::model_policy::{EscalationReason, ModelId};
use crate::session::{Session, SessionState};
use serde::{Deserialize, Serialize};

/// Default routing table embedded at compile time. Keeps the policy
/// available to every code path (chat handlers, side calls, tests)
/// without filesystem access. Operators who need to experiment can
/// set `ECAA_MODEL_POLICY_PATH=<file.yaml>` to override.
const DEFAULT_SEED_YAML: &str = include_str!("../../../../config/model-policy.yaml");

/// Typed predicate DSL parsed from `config/model-policy.yaml`.
///
/// Adding a new predicate kind: extend this enum, extend `parse`, and
/// extend `matches`. Adding a new routing rule using existing
/// predicates is a YAML row, not a Rust change.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    /// `careful_mode` — true when the session is in careful mode.
    CarefulMode,
    /// `state == blocked` — true when the session is in `Blocked`. When
    /// the rule's `one_shot_per_episode == true`, additionally
    /// requires `!session.blocked_opus_escalation_consumed`.
    StateBlocked,
    /// `confidence < <float>` — true when the session has a
    /// classification with `confidence` strictly below the threshold.
    ConfidenceLessThan(f32),
    /// `side_call_kind == <id>` — true when the eval context names a
    /// side-call kind matching `<id>` (e.g. `auto_title`,
    /// `remediation`). Always false for main-conversation evaluation.
    SideCallKind(String),
    /// `always` — unconditional fallback. Must be the last rule.
    Always,
}

/// Error returned by [`Predicate::parse`] when the predicate string is
/// syntactically invalid.
#[derive(Debug, thiserror::Error)]
pub enum PredicateParseError {
    /// The `state == <literal>` predicate used an unrecognised state name.
    #[error("unknown state literal '{0}' (only 'blocked' is supported)")]
    UnknownState(String),
    /// The `confidence < <float>` predicate had a non-numeric threshold.
    #[error("invalid confidence threshold '{0}': {1}")]
    InvalidConfidence(String, std::num::ParseFloatError),
    /// The predicate string did not match any known form.
    #[error(
        "unknown predicate '{0}' — see model_policy/registry.rs::Predicate for the supported set"
    )]
    Unknown(String),
}

impl Predicate {
    /// Parse a predicate from the DSL string stored in `model-policy.yaml`.
    pub fn parse(s: &str) -> Result<Self, PredicateParseError> {
        let s = s.trim();
        if s == "careful_mode" {
            return Ok(Self::CarefulMode);
        }
        if s == "always" {
            return Ok(Self::Always);
        }
        if let Some(rest) = s.strip_prefix("state == ") {
            return match rest.trim() {
                "blocked" => Ok(Self::StateBlocked),
                other => Err(PredicateParseError::UnknownState(other.to_string())),
            };
        }
        if let Some(rest) = s.strip_prefix("confidence < ") {
            let raw = rest.trim();
            return raw
                .parse::<f32>()
                .map(Self::ConfidenceLessThan)
                .map_err(|e| PredicateParseError::InvalidConfidence(raw.to_string(), e));
        }
        if let Some(rest) = s.strip_prefix("side_call_kind == ") {
            return Ok(Self::SideCallKind(rest.trim().to_string()));
        }
        Err(PredicateParseError::Unknown(s.to_string()))
    }

    /// Return `true` when this predicate matches the given evaluation
    /// context. `one_shot` is forwarded from the rule's
    /// `one_shot_per_episode` flag; it gates the Blocked escalation to
    /// fire at most once per blocker episode.
    pub fn matches(&self, ctx: &EvalContext, one_shot: bool) -> bool {
        match self {
            Self::CarefulMode => ctx.session.is_some_and(|s| s.careful_mode),
            Self::StateBlocked => ctx.session.is_some_and(|s| {
                let in_blocked = matches!(s.state, SessionState::Blocked { .. });
                if one_shot {
                    in_blocked && !s.blocked_opus_escalation_consumed
                } else {
                    in_blocked
                }
            }),
            Self::ConfidenceLessThan(threshold) => ctx
                .session
                .and_then(|s| s.classification.as_ref())
                // NaN confidence (deserialized from a corrupt sidecar or a
                // future producer that forgets to clamp) must trigger the
                // careful-path escalation, NOT silently fall through to
                // Sonnet. `<` returns false for NaN — fail-closed by
                // matching when confidence is NaN or below threshold.
                .is_some_and(|c| c.confidence.is_nan() || c.confidence < *threshold),
            Self::SideCallKind(want) => ctx.side_call_kind == Some(want.as_str()),
            Self::Always => true,
        }
    }
}

/// Eval context passed into `ModelRoutingTable::resolve`. The two
/// inputs are intentionally optional so the same registry can drive
/// main-conversation routing (`session: Some`, `side_call_kind: None`)
/// and side-call routing (`session: None`, `side_call_kind: Some(id)`).
pub struct EvalContext<'a> {
    /// Live session state for main-conversation routing; `None` for
    /// side-call routing where no session is in scope.
    pub session: Option<&'a Session>,
    /// Side-call kind identifier (e.g. `"auto_title"`, `"remediation"`)
    /// for routing that bypasses the main session. `None` for
    /// main-conversation turns.
    pub side_call_kind: Option<&'a str>,
}

/// `(model, reason)` decision returned by the registry.
/// `reason: None` is the Sonnet fall-through; `Some` reports an
/// escalation that `metrics.rs` attributes into the per-reason
/// counters. The internal mapping yaml-Default → None preserves the
/// legacy `choose_with_reason` contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingDecision {
    /// The model to use for this turn.
    pub model: ModelId,
    /// Escalation reason when the model differs from the Sonnet default.
    pub reason: Option<EscalationReason>,
}

/// Deserialized YAML form of a single routing rule.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RoutingRuleSpec {
    /// Predicate DSL string (e.g. `"careful_mode"`, `"confidence < 0.3"`).
    pub predicate: String,
    /// Model to use when the predicate matches.
    pub model: ModelId,
    /// Optional escalation reason label recorded in metrics.
    #[serde(default)]
    pub reason: Option<EscalationReason>,
    /// When `true`, the rule fires at most once per blocker episode
    /// (guarded by `session.blocked_opus_escalation_consumed`).
    #[serde(default)]
    pub one_shot_per_episode: bool,
}

/// Validated, runtime form of a routing rule.
#[derive(Debug, Clone)]
pub struct RoutingRule {
    /// Compiled predicate matched against each `EvalContext`.
    pub predicate: Predicate,
    /// Model selected when the predicate matches.
    pub model: ModelId,
    /// Optional escalation reason label recorded in metrics.
    pub reason: Option<EscalationReason>,
    /// When `true`, the rule fires at most once per blocker episode.
    pub one_shot_per_episode: bool,
}

/// Raw YAML envelope for the model-policy file.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelRoutingTableSpec {
    /// Ordered list of routing rule specs; evaluated top-to-bottom.
    pub rules: Vec<RoutingRuleSpec>,
}

/// Compiled model routing table.
#[derive(Debug, Clone)]
pub struct ModelRoutingTable {
    /// Ordered routing rules; the last must be an `always` fallback.
    pub rules: Vec<RoutingRule>,
}

/// Error loading or parsing the model-routing YAML.
#[derive(Debug, thiserror::Error)]
pub enum RoutingLoadError {
    /// YAML parse error from `serde_yml`.
    #[error("parsing model-policy YAML: {0}")]
    Parse(#[from] serde_yml::Error),
    /// A predicate string in the YAML was not parseable.
    #[error("rule {index} ({raw:?}): {source}")]
    Predicate {
        /// Zero-based index of the offending rule.
        index: usize,
        /// Raw predicate string from the YAML.
        raw: String,
        /// Underlying parse error.
        #[source]
        source: PredicateParseError,
    },
    /// The YAML `rules` list was empty.
    #[error("model-policy.yaml is empty (must declare at least one rule)")]
    NoRules,
    /// The last rule in the table was not an `always` fallback.
    #[error("last rule must be `always` (got {0:?})")]
    LastRuleNotAlways(String),
    /// Filesystem read failed when loading from `ECAA_MODEL_POLICY_PATH`.
    #[error("reading {path}: {source}")]
    Io {
        /// Path that could not be read.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Apply `ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD` over every
/// `confidence < <t>` predicate in `rules`.
///
/// The threshold gates the Sonnet→Opus escalation for low-classifier-
/// confidence turns. The default lives in `config/model-policy.yaml`
/// (currently `confidence < 0.3`); operators can tune at runtime
/// without editing the YAML — handy for cost-sensitive periods where
/// raising to e.g. 0.2 keeps more turns on Sonnet, or for high-
/// reliability windows where lowering to 0.4 pushes more turns to Opus.
///
/// Invalid input (non-numeric or out of `[0.0, 1.0]`) is logged at WARN
/// and ignored; the YAML default is preserved. Unset env var is a
/// silent no-op.
fn apply_confidence_threshold_env_override(rules: &mut [RoutingRule]) {
    let Ok(raw) = std::env::var("ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD") else {
        return;
    };
    let parsed = match raw.parse::<f64>() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                value = %raw,
                error = %e,
                "ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD not a float; ignoring"
            );
            return;
        }
    };
    if !(0.0..=1.0).contains(&parsed) {
        tracing::warn!(
            value = parsed,
            "ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD out of [0, 1]; ignoring"
        );
        return;
    }
    let new_threshold = parsed as f32;
    for rule in rules.iter_mut() {
        if let Predicate::ConfidenceLessThan(t) = &mut rule.predicate {
            tracing::info!(
                yaml_value = *t,
                env_value = new_threshold,
                "ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD overriding model-policy.yaml"
            );
            *t = new_threshold;
        }
    }
}

impl ModelRoutingTable {
    /// Parse from a YAML string. Validates that the table is non-empty
    /// and that the last rule is the `always` fallback so callers
    /// always get a deterministic decision.
    ///
    /// Applies `ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD` override (if set
    /// and within `[0.0, 1.0]`) over every `confidence < <t>` predicate
    /// after the YAML-derived rules are validated. The threshold is
    /// cost-relevant: lowering it routes more uncertain decisions to
    /// Opus 4.8 (~5x Sonnet per token); raising it lets more uncertain
    /// decisions ride Sonnet.
    pub fn parse(yaml: &str) -> Result<Self, RoutingLoadError> {
        let spec: ModelRoutingTableSpec = serde_yml::from_str(yaml)?;
        if spec.rules.is_empty() {
            return Err(RoutingLoadError::NoRules);
        }
        let mut rules = Vec::with_capacity(spec.rules.len());
        for (index, r) in spec.rules.into_iter().enumerate() {
            let predicate =
                Predicate::parse(&r.predicate).map_err(|e| RoutingLoadError::Predicate {
                    index,
                    raw: r.predicate.clone(),
                    source: e,
                })?;
            rules.push(RoutingRule {
                predicate,
                model: r.model,
                reason: r.reason,
                one_shot_per_episode: r.one_shot_per_episode,
            });
        }
        let last = rules
            .last()
            .expect("non-empty after rules.is_empty() check above");
        if last.predicate != Predicate::Always {
            return Err(RoutingLoadError::LastRuleNotAlways(format!(
                "{:?}",
                last.predicate
            )));
        }
        apply_confidence_threshold_env_override(&mut rules);
        Ok(Self { rules })
    }

    /// Default seed embedded at compile time. Used by every call into
    /// `ModelPolicy::*` unless `ECAA_MODEL_POLICY_PATH` overrides.
    pub fn default_seed() -> &'static Self {
        use std::sync::OnceLock;
        static SEED: OnceLock<ModelRoutingTable> = OnceLock::new();
        SEED.get_or_init(|| {
            ModelRoutingTable::parse(DEFAULT_SEED_YAML)
                .expect("embedded model-policy.yaml seed must parse")
        })
    }

    /// Load the routing table for the current process. Reads
    /// `ECAA_MODEL_POLICY_PATH` if set, otherwise returns the embedded
    /// seed. The override path is resolved per-call (no global
    /// caching) so tests can swap policies without dancing around
    /// `OnceLock`.
    pub fn current() -> std::borrow::Cow<'static, Self> {
        if let Ok(path) = std::env::var("ECAA_MODEL_POLICY_PATH") {
            let path = std::path::PathBuf::from(path);
            match std::fs::read_to_string(&path) {
                Ok(yaml) => match Self::parse(&yaml) {
                    Ok(t) => return std::borrow::Cow::Owned(t),
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "ECAA_MODEL_POLICY_PATH parse failed; using embedded seed"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "ECAA_MODEL_POLICY_PATH read failed; using embedded seed"
                    );
                }
            }
        }
        std::borrow::Cow::Borrowed(Self::default_seed())
    }

    /// Resolve to the first matching rule's decision. Panics in
    /// debug if the table somehow lacks an `always` fallback (parse
    /// validation should make this unreachable).
    pub fn resolve(&self, ctx: &EvalContext) -> RoutingDecision {
        for rule in &self.rules {
            if rule.predicate.matches(ctx, rule.one_shot_per_episode) {
                return RoutingDecision {
                    model: rule.model,
                    reason: rule.reason,
                };
            }
        }
        debug_assert!(
            false,
            "ModelRoutingTable::resolve fell through with no matching rule"
        );
        // Fall-back fallback — Sonnet, no escalation. Only reachable
        // if parse validation was bypassed.
        RoutingDecision {
            model: ModelId::Sonnet46,
            reason: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use ecaa_workflow_core::classify::ClassificationResult;

    #[test]
    fn parses_predicate_dsl_each_kind() {
        assert_eq!(
            Predicate::parse("careful_mode").unwrap(),
            Predicate::CarefulMode
        );
        assert_eq!(Predicate::parse("always").unwrap(), Predicate::Always);
        assert_eq!(
            Predicate::parse("state == blocked").unwrap(),
            Predicate::StateBlocked
        );
        match Predicate::parse("confidence < 0.3").unwrap() {
            Predicate::ConfidenceLessThan(f) => assert!((f - 0.3).abs() < 1e-6),
            other => panic!("expected ConfidenceLessThan, got {other:?}"),
        }
        assert_eq!(
            Predicate::parse("side_call_kind == auto_title").unwrap(),
            Predicate::SideCallKind("auto_title".into())
        );
        assert!(matches!(
            Predicate::parse("nope nope").unwrap_err(),
            PredicateParseError::Unknown(_)
        ));
        assert!(matches!(
            Predicate::parse("state == foobar").unwrap_err(),
            PredicateParseError::UnknownState(_)
        ));
        assert!(matches!(
            Predicate::parse("confidence < not_a_float").unwrap_err(),
            PredicateParseError::InvalidConfidence(_, _)
        ));
    }

    #[test]
    fn embedded_seed_parses_and_validates() {
        let table = ModelRoutingTable::default_seed();
        assert!(!table.rules.is_empty(), "seed must declare rules");
        assert_eq!(
            table.rules.last().unwrap().predicate,
            Predicate::Always,
            "last rule must be the `always` fallback"
        );
    }

    #[test]
    fn rejects_table_without_always_fallback() {
        let yaml = r#"
rules:
  - predicate: "careful_mode"
    model: opus_4_8
    reason: careful_mode
"#;
        let err = ModelRoutingTable::parse(yaml).unwrap_err();
        assert!(matches!(err, RoutingLoadError::LastRuleNotAlways(_)));
    }

    #[test]
    fn rejects_empty_table() {
        let yaml = "rules: []\n";
        let err = ModelRoutingTable::parse(yaml).unwrap_err();
        assert!(matches!(err, RoutingLoadError::NoRules));
    }

    #[test]
    fn careful_mode_resolves_to_opus48() {
        let s = Session::new(true);
        let table = ModelRoutingTable::default_seed();
        let dec = table.resolve(&EvalContext {
            session: Some(&s),
            side_call_kind: None,
        });
        assert_eq!(dec.model, ModelId::Opus48);
        assert_eq!(dec.reason, Some(EscalationReason::CarefulMode));
    }

    #[test]
    fn default_session_resolves_to_sonnet() {
        let s = Session::new(false);
        let table = ModelRoutingTable::default_seed();
        let dec = table.resolve(&EvalContext {
            session: Some(&s),
            side_call_kind: None,
        });
        assert_eq!(dec.model, ModelId::Sonnet46);
        assert_eq!(dec.reason, None);
    }

    #[test]
    fn low_confidence_resolves_to_opus48() {
        let mut s = Session::new(false);
        s.classification = Some(ClassificationResult {
            confidence: 0.1,
            ..Default::default()
        });
        let table = ModelRoutingTable::default_seed();
        let dec = table.resolve(&EvalContext {
            session: Some(&s),
            side_call_kind: None,
        });
        assert_eq!(dec.model, ModelId::Opus48);
        assert_eq!(dec.reason, Some(EscalationReason::LowConfidence));
    }

    #[test]
    fn blocked_one_shot_per_episode() {
        use crate::session::SessionState;
        let mut s = Session::new(false);
        s.state = SessionState::Blocked {
            blockers: vec![],
            reason: "x".into(),
            recovery_hint: "y".into(),
            blocker_kind: None,
            context: None,
        };
        let table = ModelRoutingTable::default_seed();

        // First turn of the episode → Opus.
        let dec = table.resolve(&EvalContext {
            session: Some(&s),
            side_call_kind: None,
        });
        assert_eq!(dec.model, ModelId::Opus48);
        assert_eq!(dec.reason, Some(EscalationReason::Blocked));

        // After the consumed flag flips → Sonnet (the always fallback).
        s.blocked_opus_escalation_consumed = true;
        let dec = table.resolve(&EvalContext {
            session: Some(&s),
            side_call_kind: None,
        });
        assert_eq!(dec.model, ModelId::Sonnet46);
        assert_eq!(dec.reason, None);
    }

    #[test]
    fn side_call_auto_title_resolves_to_haiku() {
        let table = ModelRoutingTable::default_seed();
        let dec = table.resolve(&EvalContext {
            session: None,
            side_call_kind: Some("auto_title"),
        });
        assert_eq!(dec.model, ModelId::Haiku45);
        assert_eq!(dec.reason, Some(EscalationReason::SideCall));
    }

    #[test]
    fn side_call_remediation_resolves_to_opus48() {
        let table = ModelRoutingTable::default_seed();
        let dec = table.resolve(&EvalContext {
            session: None,
            side_call_kind: Some("remediation"),
        });
        assert_eq!(dec.model, ModelId::Opus48);
        assert_eq!(dec.reason, Some(EscalationReason::SideCall));
    }

    /// Drives `apply_confidence_threshold_env_override` directly so the
    /// process-wide env var doesn't bleed into other test crates running
    /// in parallel. Asserts the three branches: in-range override, out-
    /// of-range ignored, non-numeric ignored.
    ///
    /// Serialized on
    /// `ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD` so the three sequential
    /// set/remove pairs inside this test (and any future tests on the
    /// same var) can't be interleaved by `cargo test` parallel workers.
    #[serial_test::serial(ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD)]
    #[test]
    fn confidence_threshold_env_override_branches() {
        // In-range value overrides every `confidence <` predicate.
        let yaml = r#"
rules:
  - predicate: "confidence < 0.3"
    model: opus_4_8
    reason: low_confidence
  - predicate: "always"
    model: sonnet_4_6
"#;
        let mut table = ModelRoutingTable::parse(yaml).expect("parse");
        // Pre-condition: YAML default in place.
        match &table.rules[0].predicate {
            Predicate::ConfidenceLessThan(t) => assert!((t - 0.3).abs() < 1e-6),
            other => panic!("expected ConfidenceLessThan, got {other:?}"),
        }
        // In-range override.
        std::env::set_var("ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD", "0.42");
        apply_confidence_threshold_env_override(&mut table.rules);
        std::env::remove_var("ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD");
        match &table.rules[0].predicate {
            Predicate::ConfidenceLessThan(t) => assert!((t - 0.42).abs() < 1e-5),
            other => panic!("expected ConfidenceLessThan after override, got {other:?}"),
        }

        // Out-of-range is ignored — last applied value (0.42) persists.
        std::env::set_var("ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD", "1.5");
        apply_confidence_threshold_env_override(&mut table.rules);
        std::env::remove_var("ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD");
        match &table.rules[0].predicate {
            Predicate::ConfidenceLessThan(t) => assert!((t - 0.42).abs() < 1e-5),
            other => panic!("expected ConfidenceLessThan still, got {other:?}"),
        }

        // Non-numeric is ignored.
        std::env::set_var("ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD", "not_a_float");
        apply_confidence_threshold_env_override(&mut table.rules);
        std::env::remove_var("ECAA_MODEL_ROUTING_CONFIDENCE_THRESHOLD");
        match &table.rules[0].predicate {
            Predicate::ConfidenceLessThan(t) => assert!((t - 0.42).abs() < 1e-5),
            other => panic!("expected ConfidenceLessThan still, got {other:?}"),
        }
    }
}
