//! `ObservableSchema` — declares the scalar quantities an atom promises to
//! report, so a quantity quoted in a narrative can be bound to a *declared*
//! observable instead of being resolved by a name search over every scalar in
//! the package.
//!
//! Modeled on the MeasEval annotation schema (SemEval-2021 Task 8): a bare
//! quantity is not interpretable on its own. Before two numbers can be
//! compared at all, a number needs its dimensional kind, the population it is
//! a quantity *of*, and the bound it asserts (exact, one-sided, an interval,
//! or hedged). `quantity_kind` types the quantity dimensionally in the spirit
//! of a QUDT quantity kind; `population` supplies the measured entity;
//! `bound_kind` supplies the modifier.
//!
//! The failure this prevents: with nothing declared, a checker name-matches a
//! narrative quantity against scalar keys anywhere in the package and convicts
//! a correct narrative because some unrelated stage happens to expose a scalar
//! under a similar key. A declared observable turns that binding into an
//! explicit promise of the stage that owns the quantity, so a mismatch means a
//! wrong number rather than a wrong lookup.
//!
//! Modality-agnostic by construction, matching the rest of the reporting
//! contract: all domain-specific meaning enters through the declaration
//! (`key`, `population`, `unit`) and never through this module, which names no
//! term belonging to any one field of study.

use std::collections::BTreeMap;

/// The scalar observables an atom promises to report, so a narrative quantity
/// can be bound to a declared observable instead of resolved by name search.
///
/// Empty (the [`Default`]) is a legal declaration and serializes to nothing:
/// an atom that promises no scalar keeps its previous emitted bytes exactly,
/// which is what lets this hang off an existing registry entry as an optional
/// block without editing a single existing declaration.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[ts(export)]
pub struct ObservableSchema {
    /// The promised observables, in authoring order. Order is load-bearing
    /// twice over: it is the resolution order of [`ObservableSchema::get`],
    /// and it is the order every serialization emits, so an emitted package
    /// stays byte-reproducible instead of depending on iteration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observables: Vec<Observable>,
}

/// One promised scalar quantity: where its value lives, what dimensional kind
/// it is, what population it quantifies, how tightly it is asserted, and
/// whether the host can recompute it.
///
/// Every field carries a serde default. Two guarantees depend on that: a
/// declaration authored today survives the addition of any later field, and a
/// partially authored declaration parses and is then rejected by
/// [`ObservableSchema::validate`] with the offending index named — a locatable
/// error instead of an opaque parse failure.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[ts(export)]
pub struct Observable {
    /// Dotted key path into the stage's `result.json`, e.g. `counts.n_retained`.
    ///
    /// Matched literally, never fuzzily: exact-match binding is what stops a
    /// quantity from attaching itself to a similarly named scalar belonging to
    /// a different stage.
    #[serde(default)]
    pub key: String,
    /// Additional accepted key paths (agent may emit a synonym). Resolution
    /// tries `key` first, then each alias in order — the same declared-then-
    /// aliases discipline the tabular contract uses for column names. The
    /// candidates live in the atom's declaration, never in the resolving code.
    /// Empty (the default) means only `key` is accepted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Dimensional kind of the quantity. Prevents an incommensurable
    /// comparison: a cardinality and a probability-scale number can carry the
    /// same textual value and mean unrelated things, and only a declared kind
    /// makes that distinction available to a checker.
    #[serde(default)]
    pub quantity_kind: ObservableKind,
    /// What the quantity is *of*. Must reference a declared port id or an
    /// abstract population name — never a domain noun. Two quantities of the
    /// same kind over different populations are not comparable, so this is the
    /// field that keeps a checker from equating them.
    #[serde(default)]
    pub population: String,
    /// Surface forms a narrative may legitimately use for `population`.
    ///
    /// `aliases` is defined over key PATHS and cannot carry this: two names for
    /// the same counted set is a different relation from two paths to the same
    /// value. Without a channel for it, a checker comparing a narrative noun to
    /// `population` must either reject every synonym — condemning "22,369 genes
    /// retained" against a population declared as features — or accept every
    /// noun, which is what let a count of matrix cells bind to a feature total.
    ///
    /// Empty means the declaration speaks only for `population` itself, and a
    /// different noun is UNDECLARED rather than wrong: strictness is opt-in by
    /// completeness, so an author is never convicted for a synonym they were
    /// never asked to declare.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub population_terms: Vec<String>,
    /// How tightly the value is asserted. Prevents convicting a deliberately
    /// one-sided or hedged statement for failing an exact-equality check.
    #[serde(default)]
    pub bound_kind: ObservableBound,
    /// Set when the value is recomputable by the host from primary bytes, so
    /// the registry can mark it authoritative rather than agent-asserted.
    /// Defaults to `false`: an undeclared provenance is the weaker claim, so
    /// silence can never upgrade an assertion into an authoritative value.
    #[serde(default, skip_serializing_if = "is_false")]
    pub host_derivable: bool,
    /// Optional unit for non-count quantities (QUDT-style symbol or CURIE).
    /// Absent means dimensionless. A unit on a [`ObservableKind::Count`] is
    /// rejected by [`ObservableSchema::validate`], because a cardinality of a
    /// declared population has no unit to carry and a spurious one would make
    /// an incommensurable comparison look well-typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unit: Option<String>,
}

/// Skip predicate for [`Observable::host_derivable`], so the default (`false`)
/// serializes to nothing and an unchanged declaration keeps its exact bytes.
fn is_false(value: &bool) -> bool {
    !*value
}

/// The dimensional kind of a declared observable, in the spirit of a QUDT
/// quantity kind. Enforces that two quantities are only ever compared when
/// they are the same kind of thing; prevents the class of false conviction
/// where a checker matches a narrative number against a value of an unrelated
/// dimension that happens to be numerically close.
///
/// `#[non_exhaustive]` because the set of kinds grows as declarations do, and
/// growing it must stay a minor change for downstream consumers.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[ts(export)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum ObservableKind {
    /// A cardinality of the declared population. Dimensionless by
    /// construction, which is why a `unit` alongside it is a declaration bug.
    Count,
    /// An additive total accumulated over the declared population.
    Sum,
    /// A minimum or maximum over the declared population.
    Extremum,
    /// A signed or absolute difference between two quantities of one kind —
    /// the shape of any "largest deviation" or "delta" statement.
    Difference,
    /// A quotient of two quantities. Dimensionless only when both sides share
    /// a kind; otherwise `unit` states the resulting dimension.
    Ratio,
    /// A quotient constrained to the unit interval, or its percentage form
    /// when `unit` says so. Separate from [`Self::Ratio`] because a checker
    /// may bound-check it.
    Proportion,
    /// A probability-scale quantity — a tail probability or an adjusted form
    /// of one. Kept distinct from [`Self::Proportion`] so a probability is
    /// never compared against a plain fraction of a population.
    Significance,
    /// A magnitude on the stage's own effect scale, signed where that scale is
    /// signed. The scale itself is named by `unit` when it has a symbol.
    Effect,
    /// A cutoff the stage *applied*, not a measurement of the population.
    /// Distinct kind so a declared cutoff is never bound to a value it filtered.
    Threshold,
    /// Elapsed time; `unit` names the time unit, since a duration is only
    /// comparable once its unit is known.
    Duration,
    /// Unclassified. The default, so an author is never forced to mis-type a
    /// quantity in order to declare it: an `Other` observable still binds by
    /// key, it simply supports no dimensional reasoning.
    #[default]
    Other,
}

/// How tightly a declared observable's value is asserted. Enforces that a
/// bound is checked as a bound; prevents convicting a one-sided or hedged
/// statement for failing an exact-equality check, and conversely prevents an
/// exact promise from being graded loosely.
///
/// `#[non_exhaustive]` so a further modifier can be added without a breaking
/// change for downstream consumers.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[ts(export)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum ObservableBound {
    /// The value is asserted exactly. The default, because an exact promise is
    /// the strictest grading and silence must not buy leniency.
    #[default]
    Point,
    /// The value is an upper bound ("at most", "no more than").
    Upper,
    /// The value is a lower bound ("at least", "no fewer than").
    Lower,
    /// The value delimits an interval reported under one key.
    Range,
    /// The value is hedged ("about", "on the order of"), so equality must be
    /// graded with tolerance rather than exactly.
    Approximate,
}

impl ObservableSchema {
    /// Resolves a key path to its declared observable: canonical `key` matches
    /// first, in declaration order, then `aliases`, in declaration order.
    ///
    /// Canonical keys outrank aliases so one declaration's synonym can never
    /// shadow another's promised key — the property that keeps existing
    /// bindings stable when a declaration later grows an alias. Total on any
    /// declaration, validated or not; [`Self::validate`] is what additionally
    /// guarantees the answer is the *only* answer.
    pub fn get(&self, key: &str) -> Option<&Observable> {
        self.observables
            .iter()
            .find(|obs| obs.key == key)
            .or_else(|| {
                self.observables
                    .iter()
                    .find(|obs| obs.aliases.iter().any(|alias| alias == key))
            })
    }

    /// Rejects a declaration that cannot support unambiguous binding: an empty
    /// `key` (nothing to bind to), an empty `population` (a quantity of
    /// nothing, which no checker can compare), the same key path promised
    /// twice across canonical keys and aliases alike (a resolution that would
    /// silently pick one of two contracts), or a `unit` on a
    /// [`ObservableKind::Count`] (a cardinality has no unit to carry).
    ///
    /// Reports the first failure with the index of the offending observable,
    /// so a registry-load error points at a declaration rather than at a file.
    /// An empty declaration is valid — declaring nothing is not an error.
    pub fn validate(&self) -> Result<(), ObservableSchemaError> {
        // Ordered map (BTreeMap): the first-declaring index reported in a
        // duplicate error must be identical on every run.
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for (index, obs) in self.observables.iter().enumerate() {
            if obs.key.is_empty() {
                return Err(ObservableSchemaError::EmptyKey { index });
            }
            if obs.population.is_empty() {
                return Err(ObservableSchemaError::EmptyPopulation {
                    index,
                    key: obs.key.clone(),
                });
            }
            if let (ObservableKind::Count, Some(unit)) = (obs.quantity_kind, &obs.unit) {
                return Err(ObservableSchemaError::UnitOnCount {
                    index,
                    key: obs.key.clone(),
                    unit: unit.clone(),
                });
            }
            let paths =
                std::iter::once(obs.key.as_str()).chain(obs.aliases.iter().map(String::as_str));
            for path in paths {
                if let Some(first_index) = seen.insert(path, index) {
                    return Err(ObservableSchemaError::DuplicateKey {
                        key: path.to_string(),
                        first_index,
                        second_index: index,
                    });
                }
            }
        }
        Ok(())
    }
}

/// Failures returned by [`ObservableSchema::validate`]. Each names the index of
/// the offending observable so a registry-load failure is locatable.
///
/// `#[non_exhaustive]` so a later rejection reason stays a minor change for
/// downstream consumers.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ObservableSchemaError {
    /// An observable with no key path: nothing could ever bind to it.
    #[error("observable at index {index} declares an empty key")]
    EmptyKey {
        /// Position of the offending observable in the declaration.
        index: usize,
    },
    /// An observable with no population: a quantity of nothing, which cannot
    /// be compared against another quantity.
    #[error("observable {key} at index {index} declares an empty population")]
    EmptyPopulation {
        /// Position of the offending observable in the declaration.
        index: usize,
        /// Key path of the offending observable.
        key: String,
    },
    /// The same key path is promised twice (as two canonical keys, as a key
    /// and an alias, or as two aliases). Resolution would have to pick one
    /// contract and silently discard the other.
    #[error(
        "key path {key} is declared more than once \
         (first at index {first_index}, again at index {second_index})"
    )]
    DuplicateKey {
        /// The key path promised more than once.
        key: String,
        /// Position of the first observable promising it.
        first_index: usize,
        /// Position of the observable promising it again.
        second_index: usize,
    },
    /// A unit declared on a cardinality. A count of a declared population is
    /// dimensionless; a unit would make an incommensurable comparison look
    /// well-typed.
    #[error("observable {key} at index {index} is a count but declares unit {unit}")]
    UnitOnCount {
        /// Position of the offending observable in the declaration.
        index: usize,
        /// Key path of the offending observable.
        key: String,
        /// The unit that must not be there.
        unit: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        Observable, ObservableBound, ObservableKind, ObservableSchema, ObservableSchemaError,
    };

    /// A full declaration exercising every field, written the way an atom
    /// YAML would carry it.
    const FULL_YAML: &str = r#"
observables:
  - key: counts.n_retained
    aliases: [counts.retained, n_retained]
    quantity_kind: count
    population: filtered_matrix
    bound_kind: point
    host_derivable: true
  - key: qc.max_abs_difference
    quantity_kind: difference
    population: replicate_pairs
    bound_kind: upper
    unit: unit:PERCENT
"#;

    #[test]
    fn yaml_round_trip_full_declaration() {
        let parsed: ObservableSchema = serde_yaml_ng::from_str(FULL_YAML).unwrap();
        assert_eq!(parsed.observables.len(), 2, "both observables parse");

        let first = &parsed.observables[0];
        assert_eq!(first.key, "counts.n_retained", "canonical key parses");
        assert_eq!(
            first.aliases,
            vec!["counts.retained", "n_retained"],
            "aliases parse in declaration order"
        );
        assert_eq!(
            first.quantity_kind,
            ObservableKind::Count,
            "snake_case kind parses"
        );
        assert_eq!(first.population, "filtered_matrix", "population parses");
        assert_eq!(first.bound_kind, ObservableBound::Point, "bound parses");
        assert!(first.host_derivable, "host_derivable parses");
        assert!(first.unit.is_none(), "a count carries no unit");

        let second = &parsed.observables[1];
        assert_eq!(
            second.quantity_kind,
            ObservableKind::Difference,
            "second kind parses"
        );
        assert_eq!(
            second.bound_kind,
            ObservableBound::Upper,
            "one-sided bound parses"
        );
        assert_eq!(
            second.unit.as_deref(),
            Some("unit:PERCENT"),
            "unit parses on a non-count"
        );
        assert!(
            !second.host_derivable,
            "omitted host_derivable defaults to false"
        );

        let reserialized = serde_yaml_ng::to_string(&parsed).unwrap();
        let reparsed: ObservableSchema = serde_yaml_ng::from_str(&reserialized).unwrap();
        assert_eq!(reparsed, parsed, "yaml round-trip is lossless");
    }

    #[test]
    fn json_round_trip_full_declaration() {
        let parsed: ObservableSchema = serde_yaml_ng::from_str(FULL_YAML).unwrap();
        let json = serde_json::to_string(&parsed).unwrap();
        let reparsed: ObservableSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(reparsed, parsed, "json round-trip is lossless");
    }

    #[test]
    fn empty_declaration_serializes_to_nothing() {
        let empty = ObservableSchema::default();
        assert_eq!(
            serde_json::to_string(&empty).unwrap(),
            "{}",
            "an atom that declares nothing emits no fields"
        );

        let yaml = serde_yaml_ng::to_string(&empty).unwrap();
        assert!(
            !yaml.contains("observables"),
            "empty observables list is skipped, got {yaml}"
        );

        let from_empty_map: ObservableSchema = serde_yaml_ng::from_str("{}").unwrap();
        assert_eq!(from_empty_map, empty, "empty map round-trips to default");

        let from_empty_list: ObservableSchema = serde_yaml_ng::from_str("observables: []").unwrap();
        assert_eq!(
            from_empty_list, empty,
            "explicit empty list equals the default"
        );
        assert!(
            empty.validate().is_ok(),
            "declaring nothing is not a failure"
        );
    }

    #[test]
    fn omitted_fields_all_default() {
        // Nothing but the two semantically required fields: the proof that no
        // field this module adds can ever be mandatory for an author.
        let parsed: ObservableSchema =
            serde_yaml_ng::from_str("observables:\n  - key: k\n    population: p\n").unwrap();
        let obs = &parsed.observables[0];
        assert_eq!(
            obs.quantity_kind,
            ObservableKind::Other,
            "kind defaults to unclassified"
        );
        assert_eq!(
            obs.bound_kind,
            ObservableBound::Point,
            "bound defaults to exact"
        );
        assert!(obs.aliases.is_empty(), "aliases default to empty");
        assert!(!obs.host_derivable, "host_derivable defaults to false");
        assert!(obs.unit.is_none(), "unit defaults to absent");
        assert!(parsed.validate().is_ok(), "a minimal declaration is valid");
    }

    fn obs(key: &str, aliases: &[&str]) -> Observable {
        Observable {
            key: key.to_string(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            population: "input_records".to_string(),
            ..Observable::default()
        }
    }

    #[test]
    fn alias_resolution_prefers_canonical_then_declaration_order() {
        let schema = ObservableSchema {
            // The first entry claims "b" as an alias; the second owns "b" as
            // its canonical key. Canonical must win.
            observables: vec![obs("a", &["b", "c"]), obs("b", &[])],
        };

        assert_eq!(
            schema.get("a").map(|o| o.key.as_str()),
            Some("a"),
            "canonical key resolves"
        );
        assert_eq!(
            schema.get("b").map(|o| o.key.as_str()),
            Some("b"),
            "a canonical key outranks an earlier-declared alias"
        );
        assert_eq!(
            schema.get("c").map(|o| o.key.as_str()),
            Some("a"),
            "an alias resolves to its owner"
        );
        assert!(
            schema.get("missing").is_none(),
            "an undeclared key path binds to nothing"
        );
    }

    #[test]
    fn alias_resolution_scans_aliases_in_declaration_order() {
        let schema = ObservableSchema {
            observables: vec![obs("first", &["shared_alias"]), obs("second", &[])],
        };
        assert_eq!(
            schema.get("shared_alias").map(|o| o.key.as_str()),
            Some("first"),
            "the earliest declaring observable answers"
        );
    }

    #[test]
    fn rejects_empty_key() {
        let schema = ObservableSchema {
            observables: vec![obs("ok", &[]), obs("", &[])],
        };
        assert_eq!(
            schema.validate(),
            Err(ObservableSchemaError::EmptyKey { index: 1 }),
            "an unbindable observable is rejected with its index"
        );
    }

    #[test]
    fn rejects_empty_population() {
        let schema = ObservableSchema {
            observables: vec![Observable {
                key: "counts.n_total".to_string(),
                ..Observable::default()
            }],
        };
        assert_eq!(
            schema.validate(),
            Err(ObservableSchemaError::EmptyPopulation {
                index: 0,
                key: "counts.n_total".to_string(),
            }),
            "a quantity of nothing is rejected"
        );
    }

    #[test]
    fn rejects_duplicate_canonical_keys() {
        let schema = ObservableSchema {
            observables: vec![obs("dup", &[]), obs("other", &[]), obs("dup", &[])],
        };
        assert_eq!(
            schema.validate(),
            Err(ObservableSchemaError::DuplicateKey {
                key: "dup".to_string(),
                first_index: 0,
                second_index: 2,
            }),
            "one key path may not name two contracts"
        );
    }

    #[test]
    fn rejects_alias_colliding_with_another_key() {
        let schema = ObservableSchema {
            observables: vec![obs("a", &[]), obs("b", &["a"])],
        };
        assert_eq!(
            schema.validate(),
            Err(ObservableSchemaError::DuplicateKey {
                key: "a".to_string(),
                first_index: 0,
                second_index: 1,
            }),
            "an alias may not shadow another observable's key"
        );
    }

    #[test]
    fn rejects_alias_repeated_within_one_observable() {
        let schema = ObservableSchema {
            observables: vec![obs("a", &["syn", "syn"])],
        };
        assert_eq!(
            schema.validate(),
            Err(ObservableSchemaError::DuplicateKey {
                key: "syn".to_string(),
                first_index: 0,
                second_index: 0,
            }),
            "a repeated alias is still an ambiguous promise"
        );
    }

    #[test]
    fn rejects_unit_on_count() {
        let schema = ObservableSchema {
            observables: vec![Observable {
                key: "counts.n_retained".to_string(),
                population: "input_records".to_string(),
                quantity_kind: ObservableKind::Count,
                unit: Some("unit:SEC".to_string()),
                ..Observable::default()
            }],
        };
        assert_eq!(
            schema.validate(),
            Err(ObservableSchemaError::UnitOnCount {
                index: 0,
                key: "counts.n_retained".to_string(),
                unit: "unit:SEC".to_string(),
            }),
            "a cardinality may not carry a unit"
        );
    }

    #[test]
    fn accepts_unit_on_non_count() {
        let schema = ObservableSchema {
            observables: vec![Observable {
                key: "timing.elapsed".to_string(),
                population: "input_records".to_string(),
                quantity_kind: ObservableKind::Duration,
                unit: Some("unit:SEC".to_string()),
                ..Observable::default()
            }],
        };
        assert!(
            schema.validate().is_ok(),
            "a dimensional quantity may carry a unit"
        );
    }

    #[test]
    fn generalizes_to_a_non_biological_domain() {
        // The generalization proof: nothing in this module knows a field of
        // study, so a declaration authored for records-and-rates works exactly
        // like one authored for any other domain.
        let yaml = r#"
observables:
  - key: ingest.accepted_ratio
    aliases: [ingest.accept_rate]
    quantity_kind: ratio
    population: input_records
    bound_kind: lower
    host_derivable: true
  - key: ingest.n_rejected
    quantity_kind: count
    population: input_records
"#;
        let schema: ObservableSchema = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            schema.validate().is_ok(),
            "a non-biological declaration validates"
        );
        assert_eq!(
            schema.get("ingest.accept_rate").map(|o| o.quantity_kind),
            Some(ObservableKind::Ratio),
            "alias resolution is domain-independent"
        );
        assert_eq!(
            schema
                .get("ingest.n_rejected")
                .map(|o| o.population.as_str()),
            Some("input_records"),
            "an abstract population name is a first-class population"
        );

        let reparsed: ObservableSchema =
            serde_yaml_ng::from_str(&serde_yaml_ng::to_string(&schema).unwrap()).unwrap();
        assert_eq!(reparsed, schema, "round-trip is domain-independent");

        let json_reparsed: ObservableSchema =
            serde_json::from_str(&serde_json::to_string(&schema).unwrap()).unwrap();
        assert_eq!(
            json_reparsed, schema,
            "json round-trip is domain-independent"
        );
    }
}
