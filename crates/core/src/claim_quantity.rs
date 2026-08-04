//! Dimensional admissibility of a narrative-quantity → observed-field binding.
//!
//! A verifier binds a numeral found in prose to a scalar field of a
//! machine-emitted summary. That binding is only meaningful when both sides
//! denote the same *kind* of quantity: the cardinality of a population, an
//! additive aggregate over it, the value of one extreme member of it, a
//! dimensionless quotient, a contrast between two states, a tail probability,
//! a bound, or an elapsed time. Two integers that agree numerically while
//! denoting different kinds of quantity are a coincidence, and treating that
//! coincidence as corroboration is how a false verdict is manufactured.
//!
//! This module exists solely to **reject** bindings whose two sides cannot
//! denote the same quantity. It never proposes, creates, or strengthens a
//! binding: [`kinds_admissible`] returning `true` is the absence of an
//! objection, not evidence. Every classifier here therefore fails toward
//! [`QuantityKind::Unknown`], which is admissible with everything, so an
//! unrecognized wording can only cost a veto — never fabricate one.
//!
//! The vocabulary is structural and statistical only (`n_`/`count`/`num`,
//! `max`/`min`/`largest`/`smallest`, `sum`, `mean`/`median`,
//! `ratio`/`fraction`/`percent`, `diff`/`delta`, `padj`/`fdr`,
//! `threshold`/`cutoff`, `seconds`/`duration`) and never subject-matter
//! nouns, so the filter applies unchanged to every workflow modality —
//! including modalities that do not exist yet.

use std::collections::BTreeSet;
use std::sync::LazyLock;

/// The kind of quantity a numeral denotes — coarse enough to be decidable
/// from vocabulary alone, fine enough to expose the conflations that produce
/// false claim verdicts.
///
/// Every member is a *kind*, not a unit: two quantities of the same kind may
/// still disagree numerically (that is the verifier's job), but two
/// quantities of incompatible kinds can never be the same measurement.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantityKind {
    /// Cardinality of a population — how many members it has (`n_*`,
    /// `count`, `num`, `total`, `size`, `length`).
    Count,
    /// An additive aggregate of a per-member magnitude (`sum`, `cumulative`).
    /// Extensive in the same way a cardinality is, so it shares a class with
    /// [`QuantityKind::Count`].
    Sum,
    /// The value carried by one *selected* extreme member of a per-member
    /// distribution (`max`, `min`, `largest`, `smallest`). Never the size of
    /// the population it was selected from.
    Extremum,
    /// A residual between two magnitudes (`diff`, `delta`, `change`,
    /// `residual`). Signed or absolute; a contrast, not a tally.
    Difference,
    /// A dimensionless quotient of two like magnitudes (`ratio`,
    /// `quotient`).
    Ratio,
    /// A dimensionless part-of-whole fraction (`proportion`, `fraction`,
    /// `percent`, `pct`, a literal `%`).
    Proportion,
    /// A tail probability or error rate under a null model (`padj`,
    /// `p-value`, `q-value`, `fdr`, `significance level`). Deliberately
    /// distinct from [`QuantityKind::Effect`].
    Significance,
    /// The magnitude of an estimated contrast (`fold`, `log2fc`, `lfc`,
    /// `effect`, `coefficient`, `beta`, `odds`, `hazard`, `slope`).
    Effect,
    /// A bound applied to some other quantity (`threshold`, `cutoff`,
    /// `limit`, `tolerance`). A bound is stated in the units of whatever it
    /// bounds, so it borrows its dimension from the other side of a binding
    /// and is admissible with every kind.
    Threshold,
    /// Elapsed machine time (`seconds`, `ms`, `duration`, `elapsed`,
    /// `runtime`, `latency`). Calendar-scale words are excluded on purpose,
    /// because those are routinely counted rather than timed.
    Duration,
    /// Nothing was recognized, or what was recognized is
    /// dimension-preserving (a mean, median, or quantile of cardinal values
    /// is itself cardinal). Admissible with every kind, including itself —
    /// the deliberate failure mode of this module.
    Unknown,
}

/// Broad dimension family a [`QuantityKind`] belongs to. Kinds inside one
/// family can denote the same measurement; kinds in different families
/// cannot, except for the one documented bridge in [`kinds_admissible`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DimensionClass {
    /// Extensive over a population: cardinalities and additive aggregates.
    Cardinal,
    /// The value of one member picked out of a distribution.
    Selection,
    /// A magnitude comparing two states.
    Contrast,
    /// A dimensionless quotient, including tail probabilities.
    Fraction,
    /// Elapsed machine time.
    Time,
    /// Borrows its dimension from the other side of the binding, so it
    /// objects to nothing.
    Universal,
}

/// How many whitespace words immediately before the noun are inspected for a
/// modifier that changes the noun's quantity kind ("the **maximum** number of
/// … ", "**12%** of …"). Kept short on purpose: a wide window picks up
/// statistical wording that belongs to a *different* numeral in the same
/// sentence and would veto a sound binding.
const PRE_MODIFIER_WORDS: usize = 4;

/// Fewest snake_case words a vocabulary-free leaf must carry before it is
/// accepted as *naming a member* of a plural container (and therefore as
/// tallying that member's occurrences). A one- or two-word leaf under a
/// plural container is more likely an unnamed statistical symbol, which stays
/// [`QuantityKind::Unknown`].
const MEMBER_NAME_MIN_WORDS: usize = 3;

/// Statistical names for a tail probability or error rate that survive
/// snake_case flattening (`n_padj` → `n padj`) and hyphenation (`p-value` →
/// `p value`). Deliberately excludes the bare word "significant", which
/// modifies a *tally* of things far more often than it names a probability —
/// `de_summary.n_significant` is a cardinality, not a p-value.
static SIGNIFICANCE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:p\s?(?:value|val|adj(?:usted)?)s?|adj\s?p(?:\s?val(?:ue)?)?s?|padj|pvals?|q\s?val(?:ue)?s?|qvals?|fdr|false\s?discovery\s?rate|significance\s?level|alpha\s?level)\b",
    )
    .expect("static regex")
});

/// Words naming the magnitude of an estimated contrast.
static EFFECT_WORDS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    [
        "fold",
        "foldchange",
        "log2fc",
        "log2foldchange",
        "log10fc",
        "lfc",
        "effect",
        "coefficient",
        "coef",
        "beta",
        "odds",
        "hazard",
        "slope",
    ]
    .into_iter()
    .collect()
});

/// Words naming elapsed machine time. Calendar units are absent by design.
static DURATION_WORDS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    [
        "second",
        "sec",
        "ms",
        "millisecond",
        "millis",
        "microsecond",
        "duration",
        "elapsed",
        "runtime",
        "walltime",
        "latency",
    ]
    .into_iter()
    .collect()
});

/// Words naming a dimensionless quotient of two like magnitudes.
static RATIO_WORDS: LazyLock<BTreeSet<&'static str>> =
    LazyLock::new(|| ["ratio", "quotient"].into_iter().collect());

/// Words naming a dimensionless part-of-whole fraction.
static PROPORTION_WORDS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    [
        "proportion",
        "fraction",
        "frac",
        "percent",
        "percentage",
        "pct",
    ]
    .into_iter()
    .collect()
});

/// Words naming a residual between two magnitudes.
static DIFFERENCE_WORDS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    [
        "diff",
        "difference",
        "delta",
        "change",
        "residual",
        "discrepancy",
    ]
    .into_iter()
    .collect()
});

/// Words naming a bound on some other quantity.
static THRESHOLD_WORDS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    [
        "threshold",
        "cutoff",
        "tolerance",
        "limit",
        "bound",
        "floor",
        "ceiling",
    ]
    .into_iter()
    .collect()
});

/// Words naming the selection of one extreme member of a distribution.
static EXTREMUM_WORDS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    [
        "max", "maximum", "maxima", "min", "minimum", "minima", "largest", "smallest", "highest",
        "lowest", "greatest", "longest", "shortest", "extremum", "extreme", "argmax", "argmin",
    ]
    .into_iter()
    .collect()
});

/// Words naming an additive aggregate over a population. "total" is absent
/// on purpose — it labels a plain cardinality at least as often as an
/// aggregate, and both are cardinal anyway.
static SUM_WORDS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    [
        "sum",
        "summed",
        "aggregate",
        "aggregated",
        "cumulative",
        "subtotal",
    ]
    .into_iter()
    .collect()
});

/// Words naming a dimension-preserving summary of a distribution. These map
/// to [`QuantityKind::Unknown`]: the median of cardinal values is cardinal,
/// the median of durations is a duration, so committing to a kind here would
/// be a fabricated distinction.
static CENTRAL_WORDS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    [
        "mean",
        "median",
        "average",
        "avg",
        "sd",
        "std",
        "stdev",
        "stddev",
        "variance",
        "iqr",
        "quantile",
        "percentile",
        "quartile",
        "midpoint",
    ]
    .into_iter()
    .collect()
});

/// Words naming a cardinality.
static COUNT_WORDS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    [
        "n",
        "num",
        "number",
        "count",
        "counted",
        "cardinality",
        "tally",
        "total",
        "size",
        "length",
        "len",
        "nrow",
        "ncol",
    ]
    .into_iter()
    .collect()
});

/// Nouns that carry no dimension of their own ("a value of 3", "a level of
/// 5"). A numeral attached to one of these is not evidence of a cardinality,
/// so it stays [`QuantityKind::Unknown`] instead of defaulting to
/// [`QuantityKind::Count`].
static PLACEHOLDER_NOUNS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    [
        "value",
        "magnitude",
        "quantity",
        "quantities",
        "amount",
        "level",
        "score",
        "unit",
        "measurement",
        "statistic",
        "metric",
    ]
    .into_iter()
    .collect()
});

/// The quantity kind implied by the narrative clause that contains the
/// numeral, given the noun the numeral quantifies.
///
/// Resolution order, most specific evidence first:
///
/// 1. the noun itself, when the noun *is* the quantity ("a maximum absolute
///    **difference** of 0" → [`QuantityKind::Difference`]);
/// 2. a modifier in the [`PRE_MODIFIER_WORDS`]-word window immediately before
///    the noun ("the **maximum** number of blocks" →
///    [`QuantityKind::Extremum`], "**12%** of rows" →
///    [`QuantityKind::Proportion`]);
/// 3. a dimensionless placeholder noun → [`QuantityKind::Unknown`];
/// 4. otherwise [`QuantityKind::Count`] — a numeral quantifying a plain noun
///    states how many of that thing there are.
///
/// The window is narrow deliberately. Statistical wording elsewhere in the
/// clause usually belongs to a *different* numeral in the same sentence, and
/// honouring it at a distance vetoes sound bindings. An empty or
/// unrecognizable noun yields [`QuantityKind::Unknown`] rather than a guess
/// taken from the clause as a whole.
#[must_use]
pub fn kind_of_clause(clause: &str, noun: &str) -> QuantityKind {
    if let Some(kind) = classify_text(noun) {
        return kind;
    }
    let bare = bare_noun(noun);
    if bare.is_empty() {
        return QuantityKind::Unknown;
    }
    if let Some(window) = pre_modifier_window(clause, &bare) {
        if let Some(kind) = classify_text(&window) {
            return kind;
        }
    }
    if tokens(&bare).iter().any(|token| is_placeholder_noun(token)) {
        return QuantityKind::Unknown;
    }
    QuantityKind::Count
}

/// The quantity kind implied by a dotted summary-field path, e.g.
/// `counts.n_features_retained` or
/// `recomputed_population_transition.max_removed_row_sum`.
///
/// Resolution order, most specific segment first:
///
/// 1. the leaf segment's own vocabulary — an outer operator wins over an
///    inner one, so `max_removed_row_sum` is an [`QuantityKind::Extremum`]
///    *of* sums and never a [`QuantityKind::Sum`];
/// 2. the container segments, nearest first (`thresholds.alpha` is a bound);
/// 3. a vocabulary-free descriptive leaf under a plural container, which
///    names a member of that collection and therefore tallies it —
///    `assertion_families.required_input_stage_and_port_binding` is a
///    [`QuantityKind::Count`] of assertions in that family;
/// 4. otherwise [`QuantityKind::Unknown`].
///
/// Dimension-bearing vocabulary outranks aggregation vocabulary, so
/// `max_elapsed_seconds` is a [`QuantityKind::Duration`] (its unit survives
/// the maximization) while `max_removed_row_sum` — whose inner quantity is
/// unnamed — stays a [`QuantityKind::Extremum`].
#[must_use]
pub fn kind_of_field(field_path: &str) -> QuantityKind {
    let segments: Vec<&str> = field_path
        .split('.')
        .filter(|segment| !segment.trim().is_empty())
        .collect();
    let Some((leaf, containers)) = segments.split_last() else {
        return QuantityKind::Unknown;
    };
    if let Some(kind) = classify_text(leaf) {
        return kind;
    }
    for container in containers.iter().rev() {
        if let Some(kind) = classify_text(container) {
            return kind;
        }
    }
    if names_collection_member(leaf, containers) {
        return QuantityKind::Count;
    }
    QuantityKind::Unknown
}

/// Whether binding a clause of kind `claim` to a field of kind `field` is
/// dimensionally admissible.
///
/// `true` is the **absence of an objection**, never support for the binding —
/// the caller must still establish it by other means. `false` is a veto: the
/// two sides cannot denote the same measurement, so numeric agreement between
/// them carries no information.
///
/// The relation is symmetric and reflexive. [`QuantityKind::Unknown`]
/// (nothing recognized) and [`QuantityKind::Threshold`] (a bound stated in
/// the units of whatever it bounds) are admissible with every kind. Kinds
/// inside one [`DimensionClass`] are mutually admissible. Exactly one
/// cross-class bridge is allowed: a contrast and a dimensionless quotient
/// interconvert across scales (an additive contrast of logarithms is a
/// multiplicative one of the underlying magnitudes), *unless* one side is
/// [`QuantityKind::Significance`] — a tail probability under a null model is
/// never a contrast magnitude, and conflating the two is the most damaging
/// misbinding this filter can catch.
#[must_use]
pub fn kinds_admissible(claim: QuantityKind, field: QuantityKind) -> bool {
    if claim == field {
        return true;
    }
    let (left, right) = (class_of(claim), class_of(field));
    if left == DimensionClass::Universal || right == DimensionClass::Universal {
        return true;
    }
    if left == right {
        return true;
    }
    let bridged = matches!(
        (left, right),
        (DimensionClass::Contrast, DimensionClass::Fraction)
            | (DimensionClass::Fraction, DimensionClass::Contrast)
    );
    bridged && claim != QuantityKind::Significance && field != QuantityKind::Significance
}

/// The dimension family of a kind. Grouping is what keeps the admissibility
/// relation small enough to justify pair by pair.
fn class_of(kind: QuantityKind) -> DimensionClass {
    match kind {
        QuantityKind::Count | QuantityKind::Sum => DimensionClass::Cardinal,
        QuantityKind::Extremum => DimensionClass::Selection,
        QuantityKind::Difference | QuantityKind::Effect => DimensionClass::Contrast,
        QuantityKind::Ratio | QuantityKind::Proportion | QuantityKind::Significance => {
            DimensionClass::Fraction
        }
        QuantityKind::Duration => DimensionClass::Time,
        QuantityKind::Threshold | QuantityKind::Unknown => DimensionClass::Universal,
    }
}

/// Classify an arbitrary fragment — a noun, a pre-modifier window, or one
/// path segment — returning `None` when no vocabulary matched at all.
///
/// `None` and `Some(Unknown)` are distinct on purpose: `None` means "keep
/// looking at less specific evidence", while `Some(Unknown)` means
/// "recognized, and deliberately non-committal" and stops the search.
fn classify_text(text: &str) -> Option<QuantityKind> {
    if text.trim().is_empty() {
        return None;
    }
    let flattened = text.to_lowercase().replace(['_', '-', '.', '/', ':'], " ");
    if SIGNIFICANCE_RE.is_match(&flattened) {
        return Some(QuantityKind::Significance);
    }
    if flattened.contains('%') {
        return Some(QuantityKind::Proportion);
    }
    let found = tokens(&flattened);
    for (words, kind) in [
        (&*EFFECT_WORDS, QuantityKind::Effect),
        (&*DURATION_WORDS, QuantityKind::Duration),
        (&*RATIO_WORDS, QuantityKind::Ratio),
        (&*PROPORTION_WORDS, QuantityKind::Proportion),
        (&*DIFFERENCE_WORDS, QuantityKind::Difference),
        (&*THRESHOLD_WORDS, QuantityKind::Threshold),
        (&*EXTREMUM_WORDS, QuantityKind::Extremum),
        (&*SUM_WORDS, QuantityKind::Sum),
        (&*CENTRAL_WORDS, QuantityKind::Unknown),
        (&*COUNT_WORDS, QuantityKind::Count),
    ] {
        if found.iter().any(|token| in_vocabulary(token, words)) {
            return Some(kind);
        }
    }
    None
}

/// Lowercased alphanumeric tokens of a fragment. Splitting on every
/// non-alphanumeric character makes snake_case, kebab-case, dotted paths, and
/// prose punctuation tokenize identically.
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Vocabulary lookup that also accepts the regular plural of a listed word,
/// so "sums", "counts", and "seconds" match without duplicating each entry.
fn in_vocabulary(token: &str, words: &BTreeSet<&'static str>) -> bool {
    if words.contains(token) {
        return true;
    }
    token
        .strip_suffix('s')
        .is_some_and(|singular| words.contains(singular))
}

/// Whether a noun token carries no dimension of its own.
fn is_placeholder_noun(token: &str) -> bool {
    if PLACEHOLDER_NOUNS.contains(token) {
        return true;
    }
    token
        .strip_suffix('s')
        .is_some_and(|singular| PLACEHOLDER_NOUNS.contains(singular))
}

/// The noun stripped of surrounding punctuation, so a parenthesized or
/// quoted noun locates and classifies the same as a bare one.
fn bare_noun(noun: &str) -> String {
    noun.trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// The up-to-[`PRE_MODIFIER_WORDS`] words immediately before the noun's
/// occurrence in the clause, or `None` when the noun does not occur.
///
/// A whole-word occurrence is preferred so a short noun cannot match inside
/// an unrelated word; a substring occurrence is the fallback, which is what
/// lets a hyphenated compound bind to its own noun.
fn pre_modifier_window(clause: &str, bare: &str) -> Option<String> {
    let lowered = clause.to_lowercase();
    let at = word_position(&lowered, bare).or_else(|| lowered.find(bare))?;
    let before = &lowered[..at];
    let words: Vec<&str> = before.split_whitespace().collect();
    let start = words.len().saturating_sub(PRE_MODIFIER_WORDS);
    Some(words[start..].join(" "))
}

/// Byte offset of the first whole-word occurrence of `needle` in `haystack`,
/// where a word boundary is any non-alphanumeric character or an end of the
/// string. Both arguments must be lowercased by the caller.
fn word_position(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let bytes = haystack.as_bytes();
    let mut from = 0usize;
    while let Some(offset) = haystack[from..].find(needle) {
        let at = from + offset;
        let end = at + needle.len();
        let opens = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        let closes = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if opens && closes {
            return Some(at);
        }
        from = end;
    }
    None
}

/// Whether a vocabulary-free leaf names a member of a plural container, in
/// which case its scalar tallies that member's occurrences.
///
/// Both conditions are required. The plural container supplies the
/// collection; the descriptive leaf (at least [`MEMBER_NAME_MIN_WORDS`]
/// words) distinguishes a named member from a bare statistical symbol, which
/// must stay [`QuantityKind::Unknown`] so this fallback cannot veto a
/// magnitude that simply went unlabelled.
fn names_collection_member(leaf: &str, containers: &[&str]) -> bool {
    tokens(leaf).len() >= MEMBER_NAME_MIN_WORDS
        && containers
            .iter()
            .any(|container| is_plural_segment(container))
}

/// Whether a path segment's last word is a regular plural, and so names a
/// collection rather than a single thing. Latin-looking `-us`/`-is` endings
/// and doubled `-ss` are excluded because they are not plurals.
fn is_plural_segment(segment: &str) -> bool {
    let Some(last) = tokens(segment).pop() else {
        return false;
    };
    last.len() >= 4
        && last.ends_with('s')
        && !last.ends_with("ss")
        && !last.ends_with("us")
        && !last.ends_with("is")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind, for the relation-level properties. Exhaustive by hand
    /// because the enum is `#[non_exhaustive]`.
    const ALL_KINDS: [QuantityKind; 11] = [
        QuantityKind::Count,
        QuantityKind::Sum,
        QuantityKind::Extremum,
        QuantityKind::Difference,
        QuantityKind::Ratio,
        QuantityKind::Proportion,
        QuantityKind::Significance,
        QuantityKind::Effect,
        QuantityKind::Threshold,
        QuantityKind::Duration,
        QuantityKind::Unknown,
    ];

    /// End-to-end verdict for one candidate binding.
    fn admissible(clause: &str, noun: &str, field: &str) -> bool {
        kinds_admissible(kind_of_clause(clause, noun), kind_of_field(field))
    }

    /// Observed misbinding: a tally of matrix columns was bound to the
    /// largest per-row sum. A population cardinality is not the value of one
    /// extreme member of a per-row distribution.
    #[test]
    fn cardinality_is_vetoed_against_maximum_of_per_member_sums() {
        let clause = "That criterion establishes only that the removed rows summed to fewer than 10 raw counts across the 8 columns";
        assert_eq!(kind_of_clause(clause, "columns"), QuantityKind::Count);
        assert_eq!(
            kind_of_field("recomputed_population_transition.max_removed_row_sum"),
            QuantityKind::Extremum
        );
        assert!(!admissible(
            clause,
            "columns",
            "recomputed_population_transition.max_removed_row_sum"
        ));
    }

    /// Observed misbinding: the retained-row cardinality (22369) was bound to
    /// the smallest retained per-row sum (10).
    #[test]
    fn retained_cardinality_is_vetoed_against_minimum_of_per_member_sums() {
        let clause =
            "filtered_count_matrix.tsv holds the 22369 retained rows as raw integer counts";
        assert_eq!(kind_of_clause(clause, "rows"), QuantityKind::Count);
        assert!(!admissible(
            clause,
            "rows",
            "recomputed_population_transition.min_retained_row_sum"
        ));
    }

    /// Observed misbinding: the cardinality of rows entering a multiplicity
    /// correction (17165) was bound to the smallest retained per-row sum (10).
    #[test]
    fn multiplicity_input_cardinality_is_vetoed_against_minimum_per_member_sum() {
        let clause = "recomputing Benjamini-Hochberg over the 17165 rows";
        assert_eq!(kind_of_clause(clause, "rows"), QuantityKind::Count);
        assert!(!admissible(
            clause,
            "rows",
            "recomputed_population_transition.min_retained_row_sum"
        ));
    }

    /// Observed misbinding: a maximum absolute difference of 0 was bound to a
    /// field whose value (5) tallies assertions in a named family. A residual
    /// between two magnitudes is not a tally of anything.
    #[test]
    fn absolute_difference_is_vetoed_against_a_named_member_tally() {
        let clause = "All 178,952 cells equal the corresponding source raw counts with a maximum absolute difference of 0";
        assert_eq!(
            kind_of_clause(clause, "difference"),
            QuantityKind::Difference
        );
        assert_eq!(
            kind_of_field("assertion_families.required_input_stage_and_port_binding"),
            QuantityKind::Count
        );
        assert!(!admissible(
            clause,
            "difference",
            "assertion_families.required_input_stage_and_port_binding"
        ));
    }

    /// The noun as recorded by the extractor may arrive parenthesized; that
    /// must not change the verdict on the maximum-absolute-difference case.
    #[test]
    fn parenthesized_noun_classifies_as_the_bare_noun() {
        let clause = "All 178,952 cells equal the corresponding source raw counts with a maximum absolute difference of 0";
        assert_eq!(
            kind_of_clause(clause, "(difference)"),
            QuantityKind::Difference
        );
        assert!(!admissible(
            clause,
            "(difference)",
            "assertion_families.required_input_stage_and_port_binding"
        ));
    }

    /// Observed misbinding this filter honestly cannot catch: a 23-row
    /// evidence table was bound to the 63677 of `sources.n_feature_rows`.
    /// Both sides are
    /// cardinalities of different populations, which is a granularity error,
    /// not a dimensional one — the kinds agree and a separate mechanism must
    /// reject it.
    #[test]
    fn two_cardinalities_of_different_populations_stay_admissible() {
        let clause = "The 23-row literature-concordance table is at evidence-row granularity";
        assert_eq!(kind_of_clause(clause, "row"), QuantityKind::Count);
        assert_eq!(kind_of_field("sources.n_feature_rows"), QuantityKind::Count);
        assert!(admissible(clause, "row", "sources.n_feature_rows"));
    }

    /// The same honest limitation: a tally of 6 entities bound to a tally of
    /// 22369 of `counts.n_features_retained` is cardinality against
    /// cardinality.
    #[test]
    fn entity_tally_against_retained_tally_stays_admissible() {
        let clause = "each of the 6 entities' supporting PMIDs has a retained snapshot";
        assert_eq!(kind_of_clause(clause, "entities"), QuantityKind::Count);
        assert_eq!(
            kind_of_field("counts.n_features_retained"),
            QuantityKind::Count
        );
        assert!(admissible(clause, "entities", "counts.n_features_retained"));
    }

    /// Correct binding that must survive: an outlier tally against an
    /// `n_*` tally field.
    #[test]
    fn flagged_tally_binding_survives() {
        let clause = "0 of 8 samples were flagged as outliers";
        assert!(admissible(
            clause,
            "samples",
            "sample_outlier_assessment.n_samples_flagged"
        ));
    }

    /// Correct binding that must survive: a retained-entity tally against
    /// the `counts.n_features_retained` tally field.
    #[test]
    fn retained_tally_binding_survives() {
        let clause = "22369 genes were retained after prefiltering";
        assert!(admissible(clause, "genes", "counts.n_features_retained"));
    }

    /// Correct binding that must survive. The clause says "significant" and
    /// the field is named `n_significant`, but both are tallies — the word
    /// must not be treated as naming a tail probability.
    #[test]
    fn significant_tally_binding_survives() {
        let clause = "4025 of 22369 tested genes are significant";
        assert_eq!(kind_of_clause(clause, "genes"), QuantityKind::Count);
        assert_eq!(
            kind_of_field("de_summary.n_significant"),
            QuantityKind::Count
        );
        assert!(admissible(clause, "genes", "de_summary.n_significant"));
    }

    /// Correct binding that must survive: a central-tendency field is
    /// dimension-preserving, so it stays `Unknown` and objects to nothing.
    #[test]
    fn central_tendency_field_binding_survives() {
        let clause = "the median library size was 22.1 million reads";
        assert_eq!(kind_of_field("library_size_median"), QuantityKind::Unknown);
        assert!(admissible(clause, "reads", "library_size_median"));
    }

    /// A pre-modifier immediately before a plain noun promotes the clause off
    /// the cardinality default, so an extremum claim binds to an extremum
    /// field instead of being vetoed.
    #[test]
    fn adjacent_pre_modifier_promotes_a_plain_noun_to_extremum() {
        let clause = "the maximum number of blocks in any batch was 12";
        assert_eq!(kind_of_clause(clause, "blocks"), QuantityKind::Extremum);
        assert!(admissible(clause, "blocks", "batches.max_block_count"));
    }

    /// A percentage sign inside the pre-modifier window makes the quantity
    /// dimensionless, which a cardinality field cannot carry.
    #[test]
    fn percentage_pre_modifier_is_vetoed_against_a_tally_field() {
        let clause = "12% of the entries were dropped";
        assert_eq!(kind_of_clause(clause, "entries"), QuantityKind::Proportion);
        assert!(!admissible(clause, "entries", "counts.n_entries_dropped"));
    }

    /// Statistical wording far from the numeral must not be honoured: the
    /// pre-modifier window is deliberately narrow so a sound cardinality
    /// binding is not vetoed by another numeral's vocabulary.
    #[test]
    fn distant_statistical_wording_does_not_change_the_clause_kind() {
        let clause = "the total library size was 4.2 billion, and we retained 22369 entries";
        assert_eq!(kind_of_clause(clause, "entries"), QuantityKind::Count);
    }

    /// A leaf's own vocabulary outranks its container's, and the outer
    /// operator outranks the inner one.
    #[test]
    fn leaf_vocabulary_outranks_container_and_inner_operator() {
        assert_eq!(kind_of_field("blocks.max_row_sum"), QuantityKind::Extremum);
        assert_eq!(kind_of_field("blocks.total_row_sum"), QuantityKind::Sum);
        assert_eq!(kind_of_field("thresholds.alpha"), QuantityKind::Threshold);
    }

    /// Dimension-bearing vocabulary survives an aggregation, so a maximized
    /// duration is still a duration and binds to a duration claim.
    #[test]
    fn dimension_vocabulary_outranks_aggregation_vocabulary() {
        assert_eq!(
            kind_of_field("stage_timing.max_elapsed_seconds"),
            QuantityKind::Duration
        );
        assert!(kinds_admissible(
            QuantityKind::Duration,
            kind_of_field("stage_timing.max_elapsed_seconds")
        ));
    }

    /// The named-member fallback needs a descriptive leaf. A bare one- or
    /// two-word leaf under a plural container stays `Unknown` so the fallback
    /// cannot veto an unlabelled magnitude.
    #[test]
    fn bare_leaf_under_plural_container_stays_unknown() {
        assert_eq!(kind_of_field("metrics.log_odds"), QuantityKind::Effect);
        assert_eq!(kind_of_field("metrics.zeta"), QuantityKind::Unknown);
        assert_eq!(kind_of_field("recorded.gamma_xi"), QuantityKind::Unknown);
    }

    /// A tail probability is never an effect magnitude; that conflation is
    /// vetoed even though both sides are continuous statistics.
    #[test]
    fn significance_and_effect_are_inadmissible() {
        assert_eq!(
            kind_of_field("de_summary.min_padj"),
            QuantityKind::Significance
        );
        assert!(!kinds_admissible(
            QuantityKind::Effect,
            kind_of_field("de_summary.min_padj")
        ));
        assert!(!kinds_admissible(
            QuantityKind::Difference,
            QuantityKind::Significance
        ));
    }

    /// A dimensionless quotient and an additive contrast interconvert across
    /// scales, so that one bridge stays open.
    #[test]
    fn contrast_and_quotient_bridge_stays_open() {
        assert!(kinds_admissible(
            QuantityKind::Difference,
            QuantityKind::Ratio
        ));
        assert!(kinds_admissible(
            QuantityKind::Effect,
            QuantityKind::Proportion
        ));
    }

    /// The failure mode of the whole module: an unrecognized quantity objects
    /// to nothing.
    #[test]
    fn unknown_is_admissible_with_every_kind() {
        for kind in ALL_KINDS {
            assert!(kinds_admissible(QuantityKind::Unknown, kind));
            assert!(kinds_admissible(kind, QuantityKind::Unknown));
        }
    }

    /// A bound is stated in the units of whatever it bounds, so it borrows
    /// its dimension from the other side and objects to nothing.
    #[test]
    fn threshold_is_admissible_with_every_kind() {
        for kind in ALL_KINDS {
            assert!(kinds_admissible(QuantityKind::Threshold, kind));
            assert!(kinds_admissible(kind, QuantityKind::Threshold));
        }
    }

    /// Admissibility is a property of the pair, not of the argument order,
    /// and no kind is inadmissible with itself.
    #[test]
    fn admissibility_is_symmetric_and_reflexive() {
        for left in ALL_KINDS {
            assert!(kinds_admissible(left, left));
            for right in ALL_KINDS {
                assert_eq!(
                    kinds_admissible(left, right),
                    kinds_admissible(right, left),
                    "asymmetric verdict for {left:?} against {right:?}"
                );
            }
        }
    }

    /// A cardinality and an additive aggregate are both extensive over the
    /// same population, so the filter must not invent a veto between them.
    #[test]
    fn cardinality_and_aggregate_stay_admissible() {
        assert!(kinds_admissible(QuantityKind::Count, QuantityKind::Sum));
    }

    /// An empty or missing field path carries no vocabulary at all.
    #[test]
    fn empty_inputs_stay_unknown() {
        assert_eq!(kind_of_field(""), QuantityKind::Unknown);
        assert_eq!(kind_of_field("."), QuantityKind::Unknown);
        assert_eq!(kind_of_clause("some clause", ""), QuantityKind::Unknown);
        assert_eq!(
            kind_of_clause("a value of 3 was recorded", "value"),
            QuantityKind::Unknown
        );
    }
}
