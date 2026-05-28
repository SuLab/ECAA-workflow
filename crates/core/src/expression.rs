//! `ExpressionEvaluator` trait + default `cel-interpreter`
//! implementation.
//!
//! CEL is the common gate / exclusion / iteration-metric language
//! across:
//!
//! - Atom `excludes:` (S4.1) — `intake.organism.taxon_id != 9606`,
//!   etc. Composer (S7.2) skips an atom when its exclusion CEL evals
//!   true against the (intake + earlier task results) context.
//! - Archetype `when:` slot-fill conditions (S6.7) — `intake.cell_count
//! < 50000` to pick `cell_ranger_count` over `starsolo`.
//! - Iteration `metric_source` (S10.1) — `validate_clustering.result.
//! silhouette > 0.6` for `IterateUntil` convergence.
//! - Downstream-policy gates (`config/downstream-policy/*.json`).
//!
//! ## Why a trait wrapper
//!
//! `cel-interpreter` is community-maintained (FOSDEM 2026 talk
//! confirms active), not Google-blessed; round-2 §3.10 flagged 12
//! advisories incl. 2 Critical in April 2026. Wrapping every eval
//! call behind `ExpressionEvaluator` means the crate dep can be
//! swapped in one file when (a) a future Google-blessed Rust CEL
//! lands, (b) Microsoft Regorus (Rego) becomes the right shape,
//! (c) Datafun ships a stable Rust impl. Today's `CelEvaluator`
//! delegates straight to `cel-interpreter` 0.10.0 (pinned exactly
//! at the workspace dep level).
//!
//! ## Determinism
//!
//! CEL is non-Turing-complete — no while-loops, no recursion, no
//! IO. Eval is pure for fixed (program, context) pairs which
//! preserves the §3.5 determinism contract: 100× composer replay
//! produces byte-identical output (S7.15 gate).
//!
//! ## Context shape
//!
//! Composer binds three top-level identifiers per the §3.7 layer
//! diagram:
//!
//! - `intake` — `IntakeFacts` flattened to a `serde_json::Value`
//! - `<task_id>.result` — typed `AgentTaskResult` per task (S3.18)
//! - `self.<port>.<attr>` — current atom's port attributes
//!
//! `ExpressionEvaluator::eval_bool` accepts a JSON `Value` for the
//! context; the implementer is responsible for binding identifiers
//! before delegating to the underlying engine.

use anyhow::{anyhow, Context, Result};

/// Trait surface for evaluating CEL (or any future replacement)
/// expressions against a JSON context. Composer (S7.2) and the
/// archetype matcher (S6.10) call `eval_bool` only — non-bool CEL
/// is reserved for iteration metric extraction (S10.1) and
/// allocated as a separate `eval_value` method when that lands.
pub trait ExpressionEvaluator {
    /// Compile + evaluate `expression` against `context`, expecting
    /// a boolean result. Returns `Err` when the expression doesn't
    /// parse, references a binding the context doesn't carry, or
    /// resolves to a non-bool value (CEL `int`, `string`, etc.).
    /// Callers translate the error into the surrounding domain
    /// blocker (`CompositionInfeasible::excluded_paths` records
    /// the verbatim expression source for SME diagnostics).
    fn eval_bool(&self, expression: &str, context: &serde_json::Value) -> Result<bool>;
}

/// Default `cel-interpreter` v0.10.0 implementation. Created once
/// at composer-construction time; one program per atom is parsed
/// lazily on first `eval_bool` and re-used across iterations.
///
/// Today this is a thin wrapper — the crate ships a
/// `Program::compile` + `Context::add_variable` API, so we don't
/// keep a compile cache here (the composer does, keyed by atom id).
/// `Default::default()` is sufficient for construction; future
/// configuration knobs (custom functions, max-evaluation-cost,
/// etc.) flow through this struct.
#[derive(Debug, Default, Clone)]
pub struct CelEvaluator;

impl ExpressionEvaluator for CelEvaluator {
    fn eval_bool(&self, expression: &str, context: &serde_json::Value) -> Result<bool> {
        use cel_interpreter::{Context, Program, Value as CelValue};

        let program = Program::compile(expression)
            .map_err(|e| anyhow!("CEL compile failed for `{expression}`: {e:?}"))?;

        // Bind every top-level key of the context as a CEL variable.
        // Composer is responsible for shaping `context` so the keys
        // match what atom CEL expressions reference (e.g. `intake`,
        // `<task_id>`, `self`). cel-interpreter 0.10 doesn't ship
        // `From<serde_json::Value>` for `Value`, so we walk the JSON
        // tree manually via `json_to_cel`.
        let mut ctx = Context::default();
        if let Some(map) = context.as_object() {
            for (key, value) in map {
                let cel_value = json_to_cel(value);
                ctx.add_variable_from_value(key.as_str(), cel_value);
            }
        } else if !context.is_null() {
            return Err(anyhow!(
                "CEL context must be a JSON object or null, got {context}"
            ));
        }

        let result = program
            .execute(&ctx)
            .with_context(|| format!("CEL execute failed: {expression}"))?;

        match result {
            CelValue::Bool(b) => Ok(b),
            other => Err(anyhow!(
                "CEL expression `{expression}` resolved to non-bool: {other:?}"
            )),
        }
    }
}

/// Recursively translate `serde_json::Value` to `cel_interpreter::Value`.
/// cel-interpreter 0.10 doesn't ship the conversion; this is the
/// canonical mapping for our composer / archetype / iteration use:
///
/// - `Null` → `Null`
/// - `Bool(b)` → `Bool(b)`
/// - integer `Number` → `Int(i64)` when it fits, else `Float(f64)`
/// - `String(s)` → `String(Arc<String>)`
/// - `Array(vec)` → `List(Arc<Vec<Value>>)`
/// - `Object(map)` → `Map(BTreeMap<Key, Value>)` with string keys
///
/// Determinism: CEL `Map` keys come back from `json()` as
/// `Key::String(Arc<String>)`, so a serde_json `BTreeMap`-shaped
/// object round-trips byte-identically through CEL.
fn json_to_cel(value: &serde_json::Value) -> cel_interpreter::Value {
    use cel_interpreter::objects::{Key, Map};
    use cel_interpreter::Value as CelValue;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    match value {
        serde_json::Value::Null => CelValue::Null,
        serde_json::Value::Bool(b) => CelValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                CelValue::Int(i)
            } else if let Some(u) = n.as_u64() {
                CelValue::UInt(u)
            } else if let Some(f) = n.as_f64() {
                CelValue::Float(f)
            } else {
                CelValue::Null
            }
        }
        serde_json::Value::String(s) => CelValue::String(Arc::new(s.clone())),
        serde_json::Value::Array(arr) => {
            let cels: Vec<CelValue> = arr.iter().map(json_to_cel).collect();
            CelValue::List(Arc::new(cels))
        }
        serde_json::Value::Object(obj) => {
            // Build via BTreeMap for deterministic insertion. cel-interpreter's
            // Map upstream-pins its inner type to HashMap, so we honor that at
            // the boundary; but sorted insertion guarantees the materialised
            // HashMap's *contents* are reproducible across calls. The composer
            // consumes via eval_bool, which never iterates the Map, so HashMap's
            // arbitrary iteration order is not observable in current call paths.
            let sorted: BTreeMap<Key, CelValue> = obj
                .iter()
                .map(|(k, v)| (Key::String(Arc::new(k.clone())), json_to_cel(v)))
                .collect();
            let map: HashMap<Key, CelValue> = sorted.into_iter().collect();
            CelValue::Map(Map { map: Arc::new(map) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cel_map_construction_is_content_stable() {
        // Determinism contract: json_to_cel built from the same JSON must
        // produce a Map with byte-identical sorted-content view across calls.
        // Cel-interpreter's inner HashMap iteration order is not stable, so
        // we compare via a sorted Vec view instead.
        use cel_interpreter::objects::Key;
        use cel_interpreter::Value as CelValue;

        fn key_str(k: &Key) -> String {
            match k {
                Key::String(s) => format!("s:{s}"),
                Key::Int(i) => format!("i:{i}"),
                Key::Uint(u) => format!("u:{u}"),
                Key::Bool(b) => format!("b:{b}"),
            }
        }
        fn render(v: &CelValue) -> String {
            // Recursive sorted-render so nested Maps don't surface
            // cel-interpreter's HashMap iteration order in the
            // comparison. Mirrors the determinism contract: same
            // input JSON → same rendered string.
            match v {
                CelValue::Map(m) => {
                    let mut entries: Vec<(String, String)> = m
                        .map
                        .iter()
                        .map(|(k, val)| (key_str(k), render(val)))
                        .collect();
                    entries.sort();
                    let body: Vec<String> = entries
                        .into_iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect();
                    format!("{{{}}}", body.join(","))
                }
                CelValue::List(items) => {
                    let body: Vec<String> = items.iter().map(render).collect();
                    format!("[{}]", body.join(","))
                }
                other => format!("{other:?}"),
            }
        }
        fn sorted_entries(v: &CelValue) -> Vec<(String, String)> {
            let CelValue::Map(m) = v else {
                panic!("expected Map");
            };
            let mut entries: Vec<(String, String)> = m
                .map
                .iter()
                .map(|(k, val)| (key_str(k), render(val)))
                .collect();
            entries.sort();
            entries
        }

        let input = serde_json::json!({
            "z": 1, "a": "two", "m": [3, 4], "b": true, "k": null,
            "nested": {"inner_z": 0, "inner_a": "x"}
        });
        let v1 = json_to_cel(&input);
        let v2 = json_to_cel(&input);
        assert_eq!(sorted_entries(&v1), sorted_entries(&v2));
    }

    #[test]
    fn cel_evaluator_evaluates_simple_inequality() {
        let cel = CelEvaluator;
        let ctx = serde_json::json!({
            "intake": {
                "organism": {"taxon_id": 9606}
            }
        });
        // Match: organism is human → exclusion does NOT fire.
        assert!(!cel
            .eval_bool("intake.organism.taxon_id != 9606", &ctx)
            .unwrap());
        // Mismatch: organism is NOT human → exclusion DOES fire.
        let ctx = serde_json::json!({
            "intake": {
                "organism": {"taxon_id": 10090}
            }
        });
        assert!(cel
            .eval_bool("intake.organism.taxon_id != 9606", &ctx)
            .unwrap());
    }

    #[test]
    fn cel_evaluator_handles_archetype_when_clause() {
        let cel = CelEvaluator;
        let ctx = serde_json::json!({
            "intake": {"cell_count": 30000}
        });
        assert!(cel.eval_bool("intake.cell_count < 50000", &ctx).unwrap());
        let ctx = serde_json::json!({
            "intake": {"cell_count": 100000}
        });
        assert!(!cel.eval_bool("intake.cell_count < 50000", &ctx).unwrap());
    }

    #[test]
    fn cel_evaluator_unbound_identifier_surfaces_error() {
        // Reference an identifier not bound in the context. CEL
        // 0.10's parser swallows some genuine compile-time syntax
        // errors with an internal panic in antlr4rust; the runtime
        // path (UndeclaredReference) is the more reliable error
        // surface for our shape.
        let cel = CelEvaluator;
        let ctx = serde_json::json!({});
        let err = cel
            .eval_bool("undeclared_var > 0", &ctx)
            .expect_err("unbound identifier must error");
        assert!(
            err.to_string().contains("CEL execute failed"),
            "expected execute-failure context, got: {err}"
        );
    }

    #[test]
    fn cel_evaluator_non_bool_result_errors() {
        let cel = CelEvaluator;
        let ctx = serde_json::json!({});
        let err = cel
            .eval_bool("42", &ctx)
            .expect_err("non-bool result must error");
        assert!(
            err.to_string().contains("non-bool"),
            "expected non-bool error, got: {err}"
        );
    }

    #[test]
    fn cel_evaluator_supports_string_compare() {
        let cel = CelEvaluator;
        let ctx = serde_json::json!({
            "intake": {"modality": "single_cell_rnaseq"}
        });
        assert!(cel
            .eval_bool("intake.modality == \"single_cell_rnaseq\"", &ctx)
            .unwrap());
    }

    /// Determinism guarantee — eval over the same (program, context)
    /// pair MUST be byte-identical across runs. Smoke test: repeat
    /// 10× and assert results match.
    #[test]
    fn cel_evaluator_is_deterministic_across_repeated_calls() {
        let cel = CelEvaluator;
        let ctx = serde_json::json!({
            "intake": {"cell_count": 30000}
        });
        let first = cel.eval_bool("intake.cell_count < 50000", &ctx).unwrap();
        for _ in 0..9 {
            let again = cel.eval_bool("intake.cell_count < 50000", &ctx).unwrap();
            assert_eq!(
                first, again,
                "CEL eval must be deterministic across repeated calls"
            );
        }
    }
}
