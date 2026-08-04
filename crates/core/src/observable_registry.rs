//! `ObservableRegistry` — the per-stage record of every scalar number a single
//! stage actually reported, each entry tagged with the authority that stands
//! behind it.
//!
//! The failure this substrate exists to prevent: a verifier that name-matches a
//! number out of a narrative against fields in ANY stage's `result.json`,
//! package-wide, at arbitrary nesting depth. Such a verifier convicts on
//! coincidence — an unrelated stage's unrelated field happening to hold the
//! same number is treated as proof the narrative meant that field. Scoping the
//! record to ONE stage, keying every value by its FULL dotted path, and
//! recording whether the value is host-derived or merely agent-asserted turns
//! that guess into two answerable questions: "did this stage compute this
//! number at all?" ([`ObservableRegistry::contains_value`]) and "under which
//! path(s), on whose authority?" ([`ObservableRegistry::entries_with_value`]).
//!
//! Two disciplines are load-bearing:
//!
//! * **Pure.** Nothing here touches the filesystem, the network, or a clock.
//!   The caller parses `result.json` and passes the [`serde_json::Value`] in.
//!   That keeps the module unit-testable and keeps `core` synchronous.
//! * **No vocabulary.** There is no allowlist of known key names and no
//!   domain-specific key handling anywhere in this module. The registry records
//!   whatever the stage wrote, under the name the stage wrote it. All
//!   stage-specific meaning enters only through the caller-supplied
//!   [`DeclaredObservable`] projection, so an unimplemented modality is served
//!   by the same code as an implemented one.

use std::collections::BTreeMap;

/// Largest absolute difference treated as "the same number" when the compared
/// magnitudes are small. Absorbs float-formatting round-trips (a stage writing
/// `0.1 + 0.2`) without ever absorbing a genuinely different value.
const ABS_TOLERANCE: f64 = 1e-9;

/// Relative slack applied at large magnitudes, where an absolute tolerance is
/// tighter than f64 can represent. Scaled by the larger operand's magnitude.
const REL_TOLERANCE: f64 = 1e-12;

/// Hard ceiling on the effective tolerance. Two distinct integers differ by at
/// least 1, so capping below 0.5 makes the comparison INTEGER-SAFE: a count of
/// 41 can never be reported as matching a count of 42, no matter how large the
/// magnitudes involved. Without this cap the relative term would conflate
/// neighbouring integers above ~1e12.
const MAX_TOLERANCE: f64 = 0.499;

/// Maximum nesting depth walked when building a registry. Matches
/// `serde_json`'s own parser recursion limit, so any value obtained by parsing
/// `result.json` text is walked in full and the cap is unreachable; it exists
/// only so a programmatically-constructed pathological value cannot overflow
/// the stack of the synchronous compiler.
const MAX_DEPTH: usize = 128;

/// A scalar an atom declared it would report. Narrow projection of the atom's
/// declaration, supplied by the caller.
///
/// Deliberately a plain constructible struct (not `#[non_exhaustive]`): the
/// caller builds these, so sealing construction would make the projection
/// unusable from outside the crate.
///
/// `key` and every entry of `aliases` are matched against a registry entry's
/// FULL dotted path, exactly and case-sensitively. Suffix, substring, and
/// depth-blind matching are intentionally NOT performed — depth-blind name
/// matching is precisely the false-conviction mechanism this module replaces.
/// A declaration for a nested value therefore states the nested path (or lists
/// it as an alias); `aliases` is the whole escape hatch for a stage that writes
/// the value under a different path than the canonical one.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredObservable {
    /// Canonical dotted path the declaration promises.
    pub key: String,
    /// Additional dotted paths that satisfy the same declaration.
    pub aliases: Vec<String>,
    /// True when the host can recompute this value from primary bytes. Such a
    /// value is authoritative; see [`Authority::HostDerived`].
    pub host_derivable: bool,
}

/// Who stands behind a registered value.
///
/// The distinction is the point of the registry: an agent-asserted number is a
/// CLAIM about a stage's output, never evidence for it, so a downstream check
/// must not treat the two alike. `#[non_exhaustive]` because this crosses the
/// serialized-package boundary — a later authority (e.g. an independently
/// attested third party) must be addable without breaking downstream matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Authority {
    /// Recomputed by the host from primary bytes, or declared host-derivable.
    /// Usable as evidence.
    HostDerived,
    /// Taken from the stage's own JSON with nothing corroborating it. A claim.
    AgentAsserted,
}

/// One scalar the stage reported, under the path it was found at.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RegistryEntry {
    /// Dotted key path as found. Array elements appear as `path.0`, `path.1`,
    /// … so a position is as addressable as a name and two different positions
    /// can never collapse onto one key.
    pub key: String,
    /// The finite value. Non-finite values are never registered, so no consumer
    /// has to defend against `NaN`-poisoned comparisons.
    pub value: f64,
    /// Whether this value may be used as evidence or only as a claim.
    pub authority: Authority,
    /// True when the entry was declared; false when it was merely discovered
    /// in the stage's JSON. An undeclared entry is still recorded — dropping it
    /// would let a number the stage genuinely reported look, to a membership
    /// check, like a number the stage never computed.
    pub declared: bool,
}

/// Every scalar one stage reported, keyed by dotted path.
///
/// Scoped to a single `stage_id` by construction: there is no package-wide
/// constructor, so a consumer cannot accidentally match a narrative number
/// against a value some unrelated stage produced. Backed by a [`BTreeMap`], so
/// iteration and serialization are sorted and byte-reproducible — a registry
/// written into an emitted package does not perturb the byte-diff baseline.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ObservableRegistry {
    /// The single stage every entry belongs to.
    pub stage_id: String,
    entries: BTreeMap<String, RegistryEntry>,
}

impl ObservableRegistry {
    /// Build from an already-parsed `result.json` value. Pure: no filesystem.
    ///
    /// Walks the JSON recursively and records EVERY finite numeric leaf under
    /// its full dotted path, with no allowlist and no name-based filtering —
    /// completeness is the invariant, because a silently dropped leaf makes a
    /// number the stage really did compute indistinguishable from one it never
    /// computed. Entries whose dotted path matches a declared key or alias are
    /// marked `declared`, and [`Authority::HostDerived`] when that declaration
    /// says `host_derivable`; everything else is
    /// [`Authority::AgentAsserted`].
    ///
    /// Coercion rules, applied uniformly to every leaf:
    ///
    /// * JSON numbers register when finite. `NaN` and `±inf` (only reachable
    ///   through a programmatically built value) are skipped entirely.
    /// * A JSON string registers iff its trimmed form parses cleanly as a
    ///   whole number in full — `"22369"`, `"-1.5e-3"`, `" 4 "` register;
    ///   `"22369 units"`, `"1,000"`, `"0x10"`, `""` do not. `"NaN"`/`"inf"`
    ///   parse but are then rejected by the finiteness rule.
    /// * Booleans and `null` never register: a flag is not a measurement, and
    ///   coercing one to 0/1 would manufacture numbers the stage never wrote.
    ///
    /// If two paths collide (only possible when a literal object key itself
    /// contains a `.`), the first occurrence in traversal order is retained, so
    /// a literal dotted key cannot silently overwrite a genuine nested path.
    /// A scalar at the JSON root registers under the empty path.
    #[must_use]
    pub fn from_result_json(
        stage_id: &str,
        result: &serde_json::Value,
        declared: &[DeclaredObservable],
    ) -> Self {
        // Declaration lookup: canonical key and every alias map to the same
        // declaration. First declaration wins on collision, so the result is a
        // deterministic function of the caller's slice order.
        let mut index: BTreeMap<&str, &DeclaredObservable> = BTreeMap::new();
        for decl in declared {
            index.entry(decl.key.as_str()).or_insert(decl);
            for alias in &decl.aliases {
                index.entry(alias.as_str()).or_insert(decl);
            }
        }

        let mut entries: BTreeMap<String, RegistryEntry> = BTreeMap::new();
        walk(result, &mut String::new(), 0, &index, &mut entries);
        Self {
            stage_id: stage_id.to_string(),
            entries,
        }
    }

    /// Overlay values the host recomputed from primary bytes. These always win
    /// and are always [`Authority::HostDerived`], because a stage's own
    /// assertion about its output is not evidence for that output.
    ///
    /// Enforces two things. First, precedence is unconditional: an
    /// agent-asserted entry under the same key is replaced, value AND
    /// authority, so a stage cannot keep a self-reported number in play by
    /// disagreeing with the host. Second, non-finite overlays are skipped, so
    /// the "every registered value is finite" invariant holds no matter what
    /// the recomputation produced.
    ///
    /// The overlay carries no declaration context, so an existing entry keeps
    /// its `declared` flag and a key introduced here is marked `declared:
    /// false` — the flag never claims a declaration that was not supplied.
    pub fn overlay_host_derived(&mut self, values: &BTreeMap<String, f64>) {
        for (key, value) in values {
            if !value.is_finite() {
                continue;
            }
            match self.entries.get_mut(key) {
                Some(existing) => {
                    existing.value = *value;
                    existing.authority = Authority::HostDerived;
                }
                None => {
                    self.entries.insert(
                        key.clone(),
                        RegistryEntry {
                            key: key.clone(),
                            value: *value,
                            authority: Authority::HostDerived,
                            declared: false,
                        },
                    );
                }
            }
        }
    }

    /// Exact dotted-path lookup. Exact by design: a fuzzy lookup would let a
    /// consumer resolve a path the stage never wrote and attribute a value to
    /// it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&RegistryEntry> {
        self.entries.get(key)
    }

    /// Every entry whose value equals `value` within an integer-safe tolerance.
    /// Used by a membership check that asks "did this stage compute this number
    /// at all?" rather than "which field does this number mean?".
    ///
    /// Returns ALL matches, sorted by key. A real registry holds many equal
    /// values, and returning only the first would let a caller bind a number to
    /// an arbitrary one of them and then report that binding as the number's
    /// meaning — the exact false conviction this module removes. `value` being
    /// non-finite yields an empty result rather than a spurious match.
    #[must_use]
    pub fn entries_with_value(&self, value: f64) -> Vec<&RegistryEntry> {
        self.entries
            .values()
            .filter(|entry| values_equal(entry.value, value))
            .collect()
    }

    /// Whether this stage reported `value` under any path. The cheap membership
    /// question; use [`Self::entries_with_value`] when the paths or their
    /// authorities matter.
    #[must_use]
    pub fn contains_value(&self, value: f64) -> bool {
        self.entries
            .values()
            .any(|entry| values_equal(entry.value, value))
    }

    /// Number of registered entries (distinct dotted paths, not distinct
    /// values).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the stage reported no usable scalar at all. Worth checking
    /// before a membership verdict: an empty registry cannot corroborate
    /// anything, and treating "no entries" as "value absent" would let a stage
    /// that wrote nothing pass as a stage that contradicted the narrative.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in sorted dotted-path order. Sorted, not insertion-ordered, so
    /// any report derived from an iteration is byte-reproducible.
    pub fn iter(&self) -> impl Iterator<Item = &RegistryEntry> {
        self.entries.values()
    }
}

/// Recursive walker. Appends to `path` in place and truncates on the way out,
/// so no intermediate path allocations accumulate and sibling branches cannot
/// observe each other's suffixes.
fn walk(
    value: &serde_json::Value,
    path: &mut String,
    depth: usize,
    index: &BTreeMap<&str, &DeclaredObservable>,
    entries: &mut BTreeMap<String, RegistryEntry>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let restore = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
                walk(child, path, depth + 1, index, entries);
                path.truncate(restore);
            }
        }
        serde_json::Value::Array(items) => {
            for (position, child) in items.iter().enumerate() {
                let restore = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(&position.to_string());
                walk(child, path, depth + 1, index, entries);
                path.truncate(restore);
            }
        }
        _ => {
            if let Some(scalar) = coerce_scalar(value) {
                let declaration = index.get(path.as_str());
                let entry = RegistryEntry {
                    key: path.clone(),
                    value: scalar,
                    authority: match declaration {
                        Some(decl) if decl.host_derivable => Authority::HostDerived,
                        _ => Authority::AgentAsserted,
                    },
                    declared: declaration.is_some(),
                };
                entries.entry(path.clone()).or_insert(entry);
            }
        }
    }
}

/// Coerce one JSON leaf to a finite `f64`, or `None` when it is not a number
/// the stage can be said to have reported. Enforces the module's coercion
/// contract in one place so no caller can widen it: booleans and `null` are
/// rejected outright, a string must parse in full, and every path is gated on
/// finiteness so a `NaN` can never enter the registry and silently swallow
/// comparisons.
fn coerce_scalar(value: &serde_json::Value) -> Option<f64> {
    let candidate = match value {
        serde_json::Value::Number(number) => number.as_f64()?,
        // `parse::<f64>()` requires the WHOLE trimmed string to be numeric, so
        // a unit-suffixed or thousands-separated string is rejected rather than
        // partially consumed.
        serde_json::Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    candidate.is_finite().then_some(candidate)
}

/// Integer-safe value comparison shared by the membership helpers. Non-finite
/// operands never match, and the tolerance is capped below 0.5 so two distinct
/// integers can never compare equal at any magnitude.
fn values_equal(left: f64, right: f64) -> bool {
    if !left.is_finite() || !right.is_finite() {
        return false;
    }
    let scale = left.abs().max(right.abs());
    let tolerance = ABS_TOLERANCE.max(REL_TOLERANCE * scale).min(MAX_TOLERANCE);
    (left - right).abs() <= tolerance
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn declared(key: &str, host_derivable: bool) -> DeclaredObservable {
        DeclaredObservable {
            key: key.to_string(),
            aliases: Vec::new(),
            host_derivable,
        }
    }

    #[test]
    fn deep_nesting_yields_full_dotted_paths() {
        let registry = ObservableRegistry::from_result_json("s", &json!({"a":{"b":{"c":7}}}), &[]);
        let entry = registry.get("a.b.c").expect("a.b.c registered");
        assert!(
            (entry.value - 7.0).abs() < f64::EPSILON,
            "deep leaf keeps its value"
        );
        assert_eq!(registry.len(), 1, "exactly one leaf registered");
        assert!(
            registry.get("c").is_none(),
            "terminal segment alone is not a key: depth-blind lookup must fail"
        );
    }

    #[test]
    fn arrays_register_by_position() {
        let registry =
            ObservableRegistry::from_result_json("s", &json!({"xs":[10, 20, {"y": 30}]}), &[]);
        assert_eq!(registry.len(), 3, "every array leaf registered");
        let keys: Vec<&str> = registry.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["xs.0", "xs.1", "xs.2.y"],
            "positions addressable and sorted"
        );
    }

    #[test]
    fn numeric_strings_accepted_and_suffixed_strings_rejected() {
        let registry = ObservableRegistry::from_result_json(
            "s",
            &json!({
                "clean": "22369",
                "signed": "-1.5e-3",
                "padded": " 4 ",
                "suffixed": "22369 units",
                "grouped": "1,000",
                "hex": "0x10",
                "empty": "",
                "words": "many"
            }),
            &[],
        );
        assert_eq!(registry.len(), 3, "only cleanly-parsing strings register");
        assert!(registry.contains_value(22369.0), "\"22369\" registered");
        assert!(registry.get("signed").is_some(), "exponent form registered");
        assert!(
            registry.get("padded").is_some(),
            "surrounding space trimmed"
        );
        for rejected in ["suffixed", "grouped", "hex", "empty", "words"] {
            assert!(
                registry.get(rejected).is_none(),
                "non-numeric string must not register: {rejected}"
            );
        }
    }

    #[test]
    fn non_finite_values_are_skipped() {
        let mut map = serde_json::Map::new();
        map.insert(
            "nan".to_string(),
            serde_json::Value::from(f64::NAN.to_string()),
        );
        map.insert(
            "inf".to_string(),
            serde_json::Value::from(f64::INFINITY.to_string()),
        );
        map.insert("neg_inf".to_string(), serde_json::Value::from("-inf"));
        map.insert("ok".to_string(), serde_json::Value::from(1.0));
        let registry =
            ObservableRegistry::from_result_json("s", &serde_json::Value::Object(map), &[]);
        assert_eq!(registry.len(), 1, "only the finite leaf registered");
        assert!(registry.get("ok").is_some(), "finite leaf survived");
        assert!(
            !registry.contains_value(f64::NAN),
            "NaN never matches, even as a query"
        );
        assert!(
            !registry.contains_value(f64::INFINITY),
            "infinite query never matches"
        );
    }

    #[test]
    fn booleans_and_nulls_are_skipped() {
        let registry = ObservableRegistry::from_result_json(
            "s",
            &json!({"flag": true, "off": false, "missing": null, "n": 0}),
            &[],
        );
        assert_eq!(registry.len(), 1, "only the numeric leaf registered");
        assert!(registry.get("n").is_some(), "numeric zero registered");
        assert!(
            registry.get("flag").is_none() && registry.get("off").is_none(),
            "booleans are not measurements"
        );
        assert!(registry.get("missing").is_none(), "null is not a value");
    }

    #[test]
    fn declared_and_discovered_entries_are_distinguished() {
        let declarations = vec![
            declared("summary.total", true),
            declared("summary.rate", false),
        ];
        let registry = ObservableRegistry::from_result_json(
            "s",
            &json!({"summary": {"total": 12, "rate": 0.5, "extra": 3}}),
            &declarations,
        );

        let total = registry.get("summary.total").expect("declared key present");
        assert!(total.declared, "declared key marked declared");
        assert_eq!(
            total.authority,
            Authority::HostDerived,
            "host_derivable declaration confers HostDerived"
        );

        let rate = registry.get("summary.rate").expect("declared key present");
        assert!(rate.declared, "declared key marked declared");
        assert_eq!(
            rate.authority,
            Authority::AgentAsserted,
            "declaration without host_derivable stays a claim"
        );

        let extra = registry
            .get("summary.extra")
            .expect("discovered key present");
        assert!(!extra.declared, "undeclared key marked discovered");
        assert_eq!(
            extra.authority,
            Authority::AgentAsserted,
            "discovered key is never authoritative"
        );
    }

    #[test]
    fn alias_resolves_a_declaration() {
        let declarations = vec![DeclaredObservable {
            key: "counts.total".to_string(),
            aliases: vec!["totals.overall".to_string(), "n_total".to_string()],
            host_derivable: true,
        }];
        let registry = ObservableRegistry::from_result_json(
            "s",
            &json!({"totals": {"overall": 99}, "n_total": 99, "other": 99}),
            &declarations,
        );

        for alias_path in ["totals.overall", "n_total"] {
            let entry = registry.get(alias_path).expect("alias path registered");
            assert!(entry.declared, "alias satisfies the declaration");
            assert_eq!(
                entry.authority,
                Authority::HostDerived,
                "alias inherits the declaration's authority"
            );
        }
        let other = registry.get("other").expect("unrelated path registered");
        assert!(
            !other.declared,
            "an equal value under an undeclared path is not promoted"
        );
    }

    #[test]
    fn overlay_overrides_an_agent_assertion_and_flips_authority() {
        let mut registry = ObservableRegistry::from_result_json(
            "s",
            &json!({"summary": {"total": 41}}),
            &[declared("summary.total", false)],
        );
        assert_eq!(
            registry.get("summary.total").expect("present").authority,
            Authority::AgentAsserted,
            "starts as a claim"
        );

        let mut host = BTreeMap::new();
        host.insert("summary.total".to_string(), 42.0);
        host.insert("summary.recomputed_only".to_string(), 7.0);
        host.insert("summary.bad".to_string(), f64::NAN);
        registry.overlay_host_derived(&host);

        let overridden = registry.get("summary.total").expect("present");
        assert!(
            (overridden.value - 42.0).abs() < f64::EPSILON,
            "host value wins over the stage's own assertion"
        );
        assert_eq!(
            overridden.authority,
            Authority::HostDerived,
            "authority flips to host-derived"
        );
        assert!(
            overridden.declared,
            "existing declaration flag is preserved by the overlay"
        );
        assert!(
            !registry.contains_value(41.0),
            "the superseded assertion is gone"
        );

        let fresh = registry.get("summary.recomputed_only").expect("present");
        assert_eq!(
            fresh.authority,
            Authority::HostDerived,
            "overlay-only key is host-derived"
        );
        assert!(
            !fresh.declared,
            "overlay carries no declaration context, so declared stays false"
        );
        assert!(
            registry.get("summary.bad").is_none(),
            "non-finite overlay is skipped"
        );
    }

    #[test]
    fn entries_with_value_returns_every_colliding_key() {
        let registry = ObservableRegistry::from_result_json(
            "s",
            &json!({"a": 5, "b": {"c": 5}, "d": "5", "e": 6}),
            &[],
        );
        let hits = registry.entries_with_value(5.0);
        let keys: Vec<&str> = hits.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["a", "b.c", "d"],
            "all colliding paths returned, sorted"
        );
        assert!(registry.contains_value(6.0), "membership finds the outlier");
        assert!(
            registry.entries_with_value(7.0).is_empty(),
            "an absent value matches nothing"
        );
    }

    #[test]
    fn tolerance_is_integer_safe_at_large_magnitude() {
        let registry =
            ObservableRegistry::from_result_json("s", &json!({"big": 1_000_000_000_000_i64}), &[]);
        assert!(
            registry.contains_value(1_000_000_000_000.0),
            "exact large integer matches"
        );
        assert!(
            !registry.contains_value(1_000_000_000_001.0),
            "adjacent large integer must not match"
        );
    }

    #[test]
    fn float_round_trip_is_absorbed() {
        let registry = ObservableRegistry::from_result_json("s", &json!({"x": 0.1 + 0.2}), &[]);
        assert!(
            registry.contains_value(0.3),
            "representation noise does not defeat membership"
        );
        assert!(
            !registry.contains_value(0.30001),
            "a genuinely different value still fails"
        );
    }

    #[test]
    fn root_scalar_and_empty_input_behave() {
        let root = ObservableRegistry::from_result_json("s", &json!(5), &[]);
        assert_eq!(root.len(), 1, "a scalar root is not dropped");
        assert!(
            root.get("").is_some(),
            "root scalar registers under the empty path"
        );

        let empty = ObservableRegistry::from_result_json("s", &serde_json::Value::Null, &[]);
        assert!(empty.is_empty(), "null root registers nothing");
        assert_eq!(empty.len(), 0, "null root has no entries");
        assert!(
            !empty.contains_value(0.0),
            "an empty registry corroborates nothing"
        );
    }

    #[test]
    fn colliding_literal_dotted_key_does_not_overwrite_nested_path() {
        let mut map = serde_json::Map::new();
        map.insert("a".to_string(), json!({"b": 1}));
        map.insert("a.b".to_string(), json!(2));
        let registry =
            ObservableRegistry::from_result_json("s", &serde_json::Value::Object(map), &[]);
        assert_eq!(registry.len(), 1, "the colliding paths share one key");
        let entry = registry.get("a.b").expect("present");
        assert!(
            (entry.value - 1.0).abs() < f64::EPSILON,
            "first traversal-order occurrence is retained"
        );
    }

    #[test]
    fn serde_round_trip_preserves_the_registry() {
        let mut registry = ObservableRegistry::from_result_json(
            "s",
            &json!({"summary": {"total": 3, "rate": 0.25}, "xs": [1, 2]}),
            &[declared("summary.total", true)],
        );
        let mut host = BTreeMap::new();
        host.insert("summary.rate".to_string(), 0.5);
        registry.overlay_host_derived(&host);

        let text = serde_json::to_string(&registry).expect("serializes");
        assert!(
            text.contains("\"host_derived\""),
            "authority serializes snake_case: {text}"
        );
        let back: ObservableRegistry = serde_json::from_str(&text).expect("deserializes");
        assert_eq!(back, registry, "round-trip is lossless");
        assert_eq!(
            serde_json::to_string(&back).expect("re-serializes"),
            text,
            "re-serialization is byte-identical"
        );
    }

    #[test]
    fn generalizes_to_a_non_biological_stage() {
        let declarations = vec![
            declared("counts.n_rows_accepted", true),
            DeclaredObservable {
                key: "counts.n_rows_rejected".to_string(),
                aliases: vec!["counts.rejected".to_string()],
                host_derivable: false,
            },
        ];
        let registry = ObservableRegistry::from_result_json(
            "ingest_records",
            &json!({
                "counts": {"n_rows_accepted": 1200, "rejected": "13"},
                "timings": {"elapsed_seconds": 4.75},
                "batches": [{"n_rows_accepted": 600}, {"n_rows_accepted": 600}]
            }),
            &declarations,
        );

        assert_eq!(registry.stage_id, "ingest_records", "stage scope retained");
        assert_eq!(
            registry.len(),
            5,
            "every leaf recorded regardless of domain"
        );
        assert!(
            registry
                .get("counts.n_rows_accepted")
                .expect("present")
                .declared,
            "declared path marked"
        );
        assert_eq!(
            registry.get("counts.rejected").expect("present").authority,
            Authority::AgentAsserted,
            "alias-resolved declaration without host_derivable stays a claim"
        );
        assert!(
            !registry
                .get("batches.0.n_rows_accepted")
                .expect("present")
                .declared,
            "same terminal segment at another path is NOT promoted by depth-blind matching"
        );
        assert_eq!(
            registry.entries_with_value(600.0).len(),
            2,
            "both colliding positions are returned"
        );
    }
}
