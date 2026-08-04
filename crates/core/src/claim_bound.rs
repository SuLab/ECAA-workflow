//! Bounding sense of numerals occurring in narrative claims.
//!
//! A numeral in prose does not always assert a point quantity. "fewer than 10"
//! asserts an open upper bound, "at most 10" a closed one, "about 10" an
//! approximation with no exact truth condition at all. A verifier that compares
//! every numeral for exact equality against an observed quantity therefore
//! reports a mismatch precisely where the narrative was correct: an observed 9
//! against a claimed "fewer than 10" *satisfies* the claim, yet exact-equality
//! adjudication flags `10 vs 9`.
//!
//! The treatment here follows the set-theoretic account of numerals in TabVer
//! (Tabular Fact Verification with Natural Logic, TACL): the admissible
//! comparison for a numeral is fixed by the monotonicity of the context it sits
//! in, and an approximate numeral denotes a pragmatic halo around its value
//! rather than the value itself — so no exact comparison against it is
//! admissible at all.
//!
//! Bounding and approximation vocabulary is the *only* vocabulary this module
//! knows. It carries no nouns from any analysis domain, which is what lets it
//! adjudicate narratives from every workflow modality, including modalities not
//! yet implemented.
//!
//! Two positional invariants stop a cue from being attributed to a numeral it
//! does not govern:
//!
//! 1. A cue must occur *before* the numeral and within the same clause — the
//!    text preceding the numeral is truncated at the nearest clause delimiter,
//!    so a cue on the far side of a comma cannot bind across it.
//! 2. Only filler tokens (whitespace, determiners) may sit between the cue and
//!    the numeral, so a cue cannot reach past intervening content to capture an
//!    unrelated quantity later in the same clause.
//!
//! When more than one cue qualifies, the one nearest the numeral governs, which
//! is why "fewer than about 10" abstains rather than bounding.

use std::sync::LazyLock;

/// The bounding sense a numeral carries in its clause.
///
/// Invariant: the sense is a property of the *claim*, never of the observed
/// quantity. It fixes which comparison [`satisfies`] is permitted to make, and
/// for [`BoundKind::Approximate`] / [`BoundKind::Range`] it records that no
/// exact comparison is permitted at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BoundKind {
    /// No bounding cue governs the numeral: it asserts an exact quantity.
    Point,
    /// Open upper bound ("fewer than 10"): satisfied iff `observed < claimed`.
    Upper,
    /// Closed upper bound ("at most 10"): satisfied iff `observed <= claimed`.
    UpperInclusive,
    /// Open lower bound ("more than 10"): satisfied iff `observed > claimed`.
    Lower,
    /// Closed lower bound ("at least 10"): satisfied iff `observed >= claimed`.
    LowerInclusive,
    /// The numeral is an endpoint of an interval ("between 5 and 10"). One
    /// endpoint alone cannot adjudicate anything; use [`range_endpoints`] plus
    /// [`range_contains`].
    Range,
    /// Approximation ("about 10"): the numeral denotes a halo whose width the
    /// text never states, so every exact comparison is inadmissible.
    Approximate,
}

/// Cue table: lexical form → the bounding sense it imposes on the numeral it
/// governs. This table is the single source of the cue vocabulary — the match
/// alternation and the sense lookup are both derived from it, so the two cannot
/// drift apart.
///
/// Invariant (asserted by `cue_table_is_ordered_longest_first`): no entry is a
/// textual prefix of a later entry. Matching is leftmost-first, so a fuller
/// phrase must be listed before any phrase it extends — otherwise "no fewer
/// than" would be classified as "fewer than" and the bound direction would
/// invert.
///
/// The inclusive/exclusive split of the symbolic cues follows their standard
/// mathematical sense: `<`/`>` are strict, `<=`/`>=`/`≤`/`≥` are inclusive.
/// Reporting `≤` as strict would re-introduce the very false mismatch this
/// module exists to remove (an observed quantity exactly at a closed bound).
const CUES: &[(&str, BoundKind)] = &[
    // Comparatives that embed a shorter comparative — longest form first.
    ("greater than or equal to", BoundKind::LowerInclusive),
    ("fewer than or equal to", BoundKind::UpperInclusive),
    ("less than or equal to", BoundKind::UpperInclusive),
    ("not greater than", BoundKind::UpperInclusive),
    ("no greater than", BoundKind::UpperInclusive),
    ("not fewer than", BoundKind::LowerInclusive),
    ("not more than", BoundKind::UpperInclusive),
    ("not less than", BoundKind::LowerInclusive),
    ("no fewer than", BoundKind::LowerInclusive),
    ("no more than", BoundKind::UpperInclusive),
    ("no less than", BoundKind::LowerInclusive),
    // Bare comparatives and superlative phrases.
    ("greater than", BoundKind::Lower),
    ("fewer than", BoundKind::Upper),
    ("less than", BoundKind::Upper),
    ("more than", BoundKind::Lower),
    ("maximum of", BoundKind::UpperInclusive),
    ("minimum of", BoundKind::LowerInclusive),
    ("at most", BoundKind::UpperInclusive),
    ("at least", BoundKind::LowerInclusive),
    ("up to", BoundKind::UpperInclusive),
    ("under", BoundKind::Upper),
    ("below", BoundKind::Upper),
    ("above", BoundKind::Lower),
    ("over", BoundKind::Lower),
    // Approximation: a halo, not a value.
    ("on the order of", BoundKind::Approximate),
    ("approximately", BoundKind::Approximate),
    ("roughly", BoundKind::Approximate),
    ("around", BoundKind::Approximate),
    ("almost", BoundKind::Approximate),
    ("nearly", BoundKind::Approximate),
    ("about", BoundKind::Approximate),
    ("circa", BoundKind::Approximate),
    // Symbolic cues — no word boundary applies to these.
    ("<=", BoundKind::UpperInclusive),
    (">=", BoundKind::LowerInclusive),
    ("≤", BoundKind::UpperInclusive),
    ("≥", BoundKind::LowerInclusive),
    ("<", BoundKind::Upper),
    (">", BoundKind::Lower),
    ("~", BoundKind::Approximate),
    ("≈", BoundKind::Approximate),
    ("∼", BoundKind::Approximate),
];

/// Tokens permitted between a cue and the numeral it governs. Deliberately
/// limited to whitespace-plus-determiner filler: anything with content ends the
/// cue's reach, which is what stops a bound from binding a numeral belonging to
/// a later predication in the same clause.
const FILLER_TOKENS: &[&str] = &[
    "a", "an", "the", "some", "only", "just", "another", "total", "of",
];

/// Lexical shape of a numeral: optional sign, digit run with optional
/// thousands grouping, optional fraction, optional exponent.
const NUMERAL: &str = r"[+-]?\d+(?:,\d{3})*(?:\.\d+)?(?:[eE][+-]?\d+)?";

/// Numeral shape accepted on either side of a bare dash interval. Unsigned and
/// exponent-free on purpose: admitting an exponent would make `1e-5` parse as
/// the interval `1` to `5`, and admitting a leading sign would make every
/// hyphenated pair ambiguous with subtraction.
const PLAIN_NUMERAL: &str = r"\d+(?:,\d{3})*(?:\.\d+)?";

/// Relative slack for every comparison in [`satisfies`], with no absolute
/// floor, so it scales down for the very small magnitudes a probability-valued
/// claim carries and up for large counts.
const RELATIVE_TOLERANCE: f64 = 1e-9;

/// Hard ceiling on the slack. Distinct integers differ by at least one unit and
/// are exact in `f64`, so capping below `0.5` guarantees the tolerance can never
/// conflate two distinct integer quantities at any magnitude, while still
/// absorbing parse/round-trip artefacts (an integer arriving as
/// `8.999999999999998`) and the double-rounding of unrepresentable decimal
/// fractions.
const MAX_TOLERANCE: f64 = 0.5;

static NUMERAL_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(NUMERAL).expect("static regex"));

/// Alternation over every cue in [`CUES`], in table order. Word-initial and
/// word-final cues are `\b`-anchored so "over" does not match inside "overall";
/// symbolic cues are not, so "≤10" is recognised with no separator.
static CUE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    let alternation = CUES
        .iter()
        .map(|(form, _)| cue_pattern(form))
        .collect::<Vec<_>>()
        .join("|");
    regex::Regex::new(&format!("(?i)(?:{alternation})")).expect("static regex")
});

/// Matches the span between a cue and the numeral it governs when that span is
/// pure filler.
static FILLER_GAP_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    let alternation = FILLER_TOKENS.join("|");
    regex::Regex::new(&format!(r"(?i)^\s*(?:(?:{alternation})\s+)*$")).expect("static regex")
});

/// Explicit interval: "between X and Y" / "between X to Y".
static BETWEEN_RANGE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(&format!(
        r"(?i)\bbetween\s+({NUMERAL})\s+(?:and|to)\s+({NUMERAL})"
    ))
    .expect("static regex")
});

/// Interval written "X to Y". The leading `(?:^|[^\w.])` guard keeps the first
/// endpoint from starting inside a longer token, and requiring a numeral before
/// "to" is what stops the upper-bound cue "up to 10" being taken as an interval.
static TO_RANGE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(&format!(
        r"(?i)(?:^|[^\w.])({PLAIN_NUMERAL})\s+to\s+({NUMERAL})"
    ))
    .expect("static regex")
});

/// Interval written with a dash: "X–Y" (en dash), "X—Y" (em dash), "X-Y".
static DASH_RANGE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(&format!(
        r"(?:^|[^\w.])({PLAIN_NUMERAL})\s*[-–—]\s*({PLAIN_NUMERAL})"
    ))
    .expect("static regex")
});

/// The bounding sense of the numeral occupying `numeral_span` within `clause`.
///
/// `numeral_span` is a byte range into `clause`. Only `numeral_span.start` is
/// load-bearing (a cue governs from the left); `numeral_span.end` participates
/// only in interval-endpoint overlap. A span whose start is out of bounds or
/// not on a UTF-8 boundary yields [`BoundKind::Point`] rather than panicking —
/// the unbounded interpretation is the conservative one, since it is what an
/// exact-equality verifier would have done anyway.
///
/// Invariants enforced: the governing cue precedes the numeral, lies in the
/// same clause, and is separated from it by filler only; interval membership
/// outranks any cue, because an endpoint cannot be adjudicated on its own.
pub fn bound_of_numeral(clause: &str, numeral_span: std::ops::Range<usize>) -> BoundKind {
    if numeral_span.start > clause.len() || !clause.is_char_boundary(numeral_span.start) {
        return BoundKind::Point;
    }
    for (low, high) in interval_endpoint_spans(clause) {
        if spans_touch(&numeral_span, &low) || spans_touch(&numeral_span, &high) {
            return BoundKind::Range;
        }
    }
    let window = clause_window(&clause[..numeral_span.start]);
    governing_cue(window).unwrap_or(BoundKind::Point)
}

/// Convenience: the bounding sense of the first numeral in the clause.
///
/// Invariant: identical to calling [`bound_of_numeral`] with the span of the
/// leftmost numeral. A clause with no numeral has nothing to bound and yields
/// [`BoundKind::Point`].
pub fn bound_of_clause(clause: &str) -> BoundKind {
    match NUMERAL_RE.find(clause) {
        Some(numeral) => bound_of_numeral(clause, numeral.range()),
        None => BoundKind::Point,
    }
}

/// Does `observed` satisfy a claim of `claimed` interpreted under `bound`?
///
/// Comparisons, all evaluated with the tolerance described on
/// [`RELATIVE_TOLERANCE`] and [`MAX_TOLERANCE`]:
///
/// - [`BoundKind::Point`] → `claimed == observed`
/// - [`BoundKind::Upper`] → `observed < claimed`
/// - [`BoundKind::UpperInclusive`] → `observed <= claimed`
/// - [`BoundKind::Lower`] → `observed > claimed`
/// - [`BoundKind::LowerInclusive`] → `observed >= claimed`
///
/// Returns `None` — abstains — when the bounding sense makes any exact
/// adjudication inadmissible: [`BoundKind::Approximate`] (halo of unstated
/// width) and [`BoundKind::Range`] (a single endpoint is not a claim).
///
/// Invariant: a non-finite input is never adjudicated. `NaN` has no order and
/// an infinity admits no meaningful tolerance, so both yield `None` instead of
/// a comparison whose result would be an artefact of IEEE semantics.
pub fn satisfies(claimed: f64, observed: f64, bound: BoundKind) -> Option<bool> {
    if !claimed.is_finite() || !observed.is_finite() {
        return None;
    }
    let slack = tolerance(claimed, observed);
    // Positive when the observed quantity sits above the claimed one.
    let delta = observed - claimed;
    match bound {
        BoundKind::Point => Some(delta.abs() <= slack),
        BoundKind::Upper => Some(delta < -slack),
        BoundKind::UpperInclusive => Some(delta <= slack),
        BoundKind::Lower => Some(delta > slack),
        BoundKind::LowerInclusive => Some(delta >= -slack),
        BoundKind::Approximate | BoundKind::Range => None,
    }
}

/// For [`BoundKind::Range`]: both endpoints of the leftmost interval in
/// `clause`.
///
/// Invariant: the pair is returned normalised as `(low, high)` with
/// `low <= high`, independent of the order the two endpoints appear in the
/// text, so a caller can never invert a containment test by trusting textual
/// order. `None` when the clause states no interval, when either endpoint fails
/// to parse, or when either endpoint is non-finite.
pub fn range_endpoints(clause: &str) -> Option<(f64, f64)> {
    let (low_span, high_span) = interval_endpoint_spans(clause).into_iter().next()?;
    let first = parse_numeral(&clause[low_span])?;
    let second = parse_numeral(&clause[high_span])?;
    if !first.is_finite() || !second.is_finite() {
        return None;
    }
    Some(if first <= second {
        (first, second)
    } else {
        (second, first)
    })
}

/// Is `observed` inside the interval `endpoints`?
///
/// Invariant: containment is *inclusive* of both endpoints — a stated interval
/// is the closed set its endpoints delimit — and order-insensitive, so a caller
/// passing an unnormalised pair still gets the intended answer. A non-finite
/// input is never inside any interval.
pub fn range_contains(endpoints: (f64, f64), observed: f64) -> bool {
    let (first, second) = endpoints;
    if !first.is_finite() || !second.is_finite() || !observed.is_finite() {
        return false;
    }
    let (low, high) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };
    let slack = tolerance(low, observed).max(tolerance(high, observed));
    (low - slack..=high + slack).contains(&observed)
}

/// Comparison slack for a claimed/observed pair: relative, then capped.
fn tolerance(claimed: f64, observed: f64) -> f64 {
    (RELATIVE_TOLERANCE * claimed.abs().max(observed.abs())).min(MAX_TOLERANCE)
}

/// Match pattern for one cue form. Multi-word forms tolerate any whitespace run
/// between words; each word is escaped so a cue can never inject metacharacters
/// into the alternation.
fn cue_pattern(form: &str) -> String {
    let body = form
        .split(' ')
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(r"\s+");
    let lead = if form.starts_with(char::is_alphanumeric) {
        r"\b"
    } else {
        ""
    };
    let trail = if form.ends_with(char::is_alphanumeric) {
        r"\b"
    } else {
        ""
    };
    format!("{lead}{body}{trail}")
}

/// The bounding sense of a matched cue. Lookup is over [`CUES`] itself, so the
/// alternation and the sense mapping cannot disagree; whitespace inside a
/// multi-word match is normalised before comparison.
fn cue_sense(matched: &str) -> Option<BoundKind> {
    let normalized = matched
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    CUES.iter()
        .find(|(form, _)| *form == normalized)
        .map(|(_, sense)| *sense)
}

/// The cue governing a numeral that immediately follows `window`, or `None`
/// when no cue in the window reaches the numeral.
///
/// Matches arrive left to right and do not overlap, so the last one whose
/// trailing span is pure filler is the nearest qualifying cue — that is the one
/// that governs.
fn governing_cue(window: &str) -> Option<BoundKind> {
    let mut nearest = None;
    for matched in CUE_RE.find_iter(window) {
        if FILLER_GAP_RE.is_match(&window[matched.end()..]) {
            nearest = cue_sense(matched.as_str()).or(nearest);
        }
    }
    nearest
}

/// The tail of `prefix` that belongs to the same clause as the numeral
/// following it — everything after the last clause delimiter.
///
/// A `,` or `.` flanked by digits on both sides is a thousands separator or a
/// decimal point, not a delimiter, so numeral-internal punctuation never splits
/// a clause.
fn clause_window(prefix: &str) -> &str {
    let mut cut = 0usize;
    for (index, character) in prefix.char_indices() {
        let delimits = match character {
            ',' | '.' => {
                let previous_is_digit = prefix[..index]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_digit());
                let next_is_digit = prefix[index + character.len_utf8()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit());
                let numeral_internal = previous_is_digit && next_is_digit;
                !numeral_internal
            }
            ';' | ':' | '!' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\u{201c}'
            | '\u{201d}' | '|' | '\n' | '\r' => true,
            _ => false,
        };
        if delimits {
            cut = index + character.len_utf8();
        }
    }
    &prefix[cut..]
}

/// Byte spans of the two endpoint numerals of every interval construct in
/// `clause`, ordered by position of the first endpoint.
fn interval_endpoint_spans(clause: &str) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let mut spans: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> = Vec::new();
    for pattern in [&*BETWEEN_RANGE_RE, &*TO_RANGE_RE, &*DASH_RANGE_RE] {
        for captures in pattern.captures_iter(clause) {
            if let (Some(low), Some(high)) = (captures.get(1), captures.get(2)) {
                spans.push((low.range(), high.range()));
            }
        }
    }
    spans.sort_by_key(|(low, high)| (low.start, high.end));
    spans
}

/// Does the numeral span coincide with an interval endpoint span? Written to
/// hold for a zero-width numeral span as well, so a caller that supplies only a
/// position still resolves interval membership.
fn spans_touch(numeral: &std::ops::Range<usize>, endpoint: &std::ops::Range<usize>) -> bool {
    endpoint.contains(&numeral.start) || numeral.contains(&endpoint.start)
}

/// Parse a matched numeral, discarding thousands separators.
fn parse_numeral(raw: &str) -> Option<f64> {
    raw.replace(',', "").parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bounding sense of the numeral `needle` inside `clause`.
    fn bound_of(clause: &str, needle: &str) -> BoundKind {
        let start = clause.find(needle).expect("fixture contains the numeral");
        bound_of_numeral(clause, start..start + needle.len())
    }

    #[test]
    fn cue_table_is_ordered_longest_first() {
        for (earlier, (earlier_form, _)) in CUES.iter().enumerate() {
            for (later_form, _) in CUES.iter().skip(earlier + 1) {
                assert!(
                    !later_form.starts_with(earlier_form),
                    "cue {later_form:?} extends {earlier_form:?} but is listed after it; \
                     leftmost-first matching would classify it as the shorter cue"
                );
            }
        }
    }

    #[test]
    fn every_cue_in_the_table_is_recognised() {
        for (form, expected) in CUES {
            let clause = format!("{form} 10");
            assert_eq!(
                bound_of_clause(&clause),
                *expected,
                "cue {form:?} did not resolve to its tabled sense in {clause:?}"
            );
        }
    }

    #[test]
    fn point_is_the_default() {
        assert_eq!(
            bound_of_clause("the total was 42"),
            BoundKind::Point,
            "an uncued numeral asserts an exact quantity"
        );
        assert_eq!(
            bound_of_clause("no numeral appears here"),
            BoundKind::Point,
            "a clause with no numeral has nothing to bound"
        );
    }

    #[test]
    fn open_upper_cues() {
        for clause in [
            "fewer than 10 rows were removed",
            "less than 10 entries remained",
            "under 10 items were dropped",
            "below 10 units were recorded",
            "the cutoff was < 10 units",
        ] {
            assert_eq!(
                bound_of_clause(clause),
                BoundKind::Upper,
                "expected an open upper bound in {clause:?}"
            );
        }
    }

    #[test]
    fn closed_upper_cues() {
        for clause in [
            "no more than 10 rows were removed",
            "at most 10 entries remained",
            "up to 10 items were dropped",
            "a maximum of 10 units was recorded",
            "the cutoff was <= 10 units",
            "the cutoff was ≤ 10 units",
            "no greater than 10 units",
            "not more than 10 units",
            "less than or equal to 10 units",
            "fewer than or equal to 10 units",
        ] {
            assert_eq!(
                bound_of_clause(clause),
                BoundKind::UpperInclusive,
                "expected a closed upper bound in {clause:?}"
            );
        }
    }

    #[test]
    fn open_lower_cues() {
        for clause in [
            "more than 10 rows were retained",
            "greater than 10 entries remained",
            "over 10 items were kept",
            "above 10 units were recorded",
            "the cutoff was > 10 units",
        ] {
            assert_eq!(
                bound_of_clause(clause),
                BoundKind::Lower,
                "expected an open lower bound in {clause:?}"
            );
        }
    }

    #[test]
    fn closed_lower_cues() {
        for clause in [
            "at least 10 rows were retained",
            "no fewer than 10 entries remained",
            "a minimum of 10 items was kept",
            "the cutoff was >= 10 units",
            "the cutoff was ≥ 10 units",
            "no less than 10 units",
            "not fewer than 10 units",
            "not less than 10 units",
            "greater than or equal to 10 units",
        ] {
            assert_eq!(
                bound_of_clause(clause),
                BoundKind::LowerInclusive,
                "expected a closed lower bound in {clause:?}"
            );
        }
    }

    #[test]
    fn approximation_cues() {
        for clause in [
            "approximately 10 rows were removed",
            "about 10 rows were removed",
            "around 10 rows were removed",
            "roughly 10 rows were removed",
            "circa 10 rows were removed",
            "~10 rows were removed",
            "≈10 rows were removed",
            "nearly 10 rows were removed",
            "almost 10 rows were removed",
            "on the order of 10 rows were removed",
        ] {
            assert_eq!(
                bound_of_clause(clause),
                BoundKind::Approximate,
                "expected an approximation in {clause:?}"
            );
        }
    }

    #[test]
    fn interval_cues() {
        for clause in [
            "between 5 and 10 rows were removed",
            "between 5 to 10 rows were removed",
            "5 to 10 rows were removed",
            "5–10 rows were removed",
            "5—10 rows were removed",
            "5-10 rows were removed",
        ] {
            assert_eq!(
                bound_of_clause(clause),
                BoundKind::Range,
                "expected an interval in {clause:?}"
            );
        }
    }

    #[test]
    fn both_interval_endpoints_report_range() {
        let clause = "between 5 and 10 rows were removed";
        assert_eq!(
            bound_of(clause, "5"),
            BoundKind::Range,
            "the lower endpoint is part of the interval"
        );
        assert_eq!(
            bound_of(clause, "10"),
            BoundKind::Range,
            "the upper endpoint is part of the interval"
        );
    }

    #[test]
    fn a_cue_governs_only_a_following_numeral_in_its_own_clause() {
        let clause = "8 samples were assessed, and fewer than 10 rows were removed";
        assert_eq!(
            bound_of(clause, "8"),
            BoundKind::Point,
            "the numeral before the cue is uncued"
        );
        assert_eq!(
            bound_of(clause, "10"),
            BoundKind::Upper,
            "the cue governs the numeral it precedes"
        );
    }

    #[test]
    fn a_cue_does_not_leak_across_a_comma_boundary() {
        let clause = "fewer than 10 rows were removed, and 8 samples were assessed";
        assert_eq!(
            bound_of(clause, "8"),
            BoundKind::Point,
            "a cue in the preceding clause must not bind across the comma"
        );
        assert_eq!(
            bound_of(clause, "10"),
            BoundKind::Upper,
            "the cue still governs its own numeral"
        );
    }

    #[test]
    fn a_cue_does_not_reach_past_intervening_content() {
        let clause = "fewer than 10 rows were removed from 8 archives";
        assert_eq!(
            bound_of(clause, "8"),
            BoundKind::Point,
            "with no punctuation boundary, the filler-only rule still blocks the reach"
        );
    }

    #[test]
    fn filler_tokens_may_separate_a_cue_from_its_numeral() {
        assert_eq!(
            bound_of_clause("at most a total of 10 rows were removed"),
            BoundKind::UpperInclusive,
            "determiner filler does not break the cue's reach"
        );
    }

    #[test]
    fn the_nearest_cue_governs() {
        assert_eq!(
            bound_of_clause("fewer than approximately 10 rows were removed"),
            BoundKind::Approximate,
            "an inner approximation cue outranks the outer comparative"
        );
    }

    #[test]
    fn cues_do_not_match_inside_longer_words() {
        assert_eq!(
            bound_of_clause("overall 10 rows were removed"),
            BoundKind::Point,
            "\"over\" inside \"overall\" is not a bounding cue"
        );
    }

    #[test]
    fn numeral_internal_punctuation_is_not_a_clause_boundary() {
        assert_eq!(
            bound_of_clause("under 0.05 was recorded"),
            BoundKind::Upper,
            "a decimal point must not truncate the cue window"
        );
        assert_eq!(
            bound_of_clause("fewer than 1,000 rows were removed"),
            BoundKind::Upper,
            "a thousands separator must not truncate the cue window"
        );
    }

    #[test]
    fn scientific_notation_is_not_an_interval() {
        assert_eq!(
            bound_of_clause("the adjusted value was 1e-5"),
            BoundKind::Point,
            "an exponent is not a dash interval"
        );
        assert_eq!(
            range_endpoints("the adjusted value was 1e-5"),
            None,
            "an exponent yields no interval endpoints"
        );
    }

    #[test]
    fn an_out_of_bounds_span_is_point() {
        assert_eq!(
            bound_of_numeral("fewer than 10", 900..910),
            BoundKind::Point,
            "an out-of-bounds span must not panic"
        );
        assert_eq!(
            bound_of_numeral("fewer than ≤10", 12..13),
            BoundKind::Point,
            "a span starting mid-character must not panic"
        );
    }

    #[test]
    fn production_case_open_upper_bound_is_satisfied() {
        let clause = "the removed rows summed to fewer than 10 raw counts";
        assert_eq!(
            bound_of_clause(clause),
            BoundKind::Upper,
            "the narrative states a bound, not a point count"
        );
        assert_eq!(
            satisfies(10.0, 9.0, BoundKind::Upper),
            Some(true),
            "an observed 9 satisfies a claimed \"fewer than 10\""
        );
        assert_eq!(
            satisfies(10.0, 9.0, BoundKind::Point),
            Some(false),
            "the same pair adjudicated as a point count is the false mismatch being removed"
        );
    }

    #[test]
    fn satisfies_point() {
        assert_eq!(
            satisfies(10.0, 10.0, BoundKind::Point),
            Some(true),
            "equal quantities satisfy a point claim"
        );
        assert_eq!(
            satisfies(10.0, 11.0, BoundKind::Point),
            Some(false),
            "distinct quantities do not satisfy a point claim"
        );
    }

    #[test]
    fn satisfies_upper_bounds() {
        assert_eq!(
            satisfies(10.0, 9.0, BoundKind::Upper),
            Some(true),
            "strictly below an open upper bound"
        );
        assert_eq!(
            satisfies(10.0, 10.0, BoundKind::Upper),
            Some(false),
            "an open upper bound excludes its endpoint"
        );
        assert_eq!(
            satisfies(10.0, 10.0, BoundKind::UpperInclusive),
            Some(true),
            "a closed upper bound includes its endpoint"
        );
        assert_eq!(
            satisfies(10.0, 11.0, BoundKind::UpperInclusive),
            Some(false),
            "above a closed upper bound"
        );
    }

    #[test]
    fn satisfies_lower_bounds() {
        assert_eq!(
            satisfies(10.0, 11.0, BoundKind::Lower),
            Some(true),
            "strictly above an open lower bound"
        );
        assert_eq!(
            satisfies(10.0, 10.0, BoundKind::Lower),
            Some(false),
            "an open lower bound excludes its endpoint"
        );
        assert_eq!(
            satisfies(10.0, 10.0, BoundKind::LowerInclusive),
            Some(true),
            "a closed lower bound includes its endpoint"
        );
        assert_eq!(
            satisfies(10.0, 9.0, BoundKind::LowerInclusive),
            Some(false),
            "below a closed lower bound"
        );
    }

    #[test]
    fn satisfies_abstains_where_no_exact_comparison_is_admissible() {
        assert_eq!(
            satisfies(10.0, 9.0, BoundKind::Approximate),
            None,
            "an approximation has no exact truth condition"
        );
        assert_eq!(
            satisfies(10.0, 9.0, BoundKind::Range),
            None,
            "a single interval endpoint is not a claim"
        );
    }

    #[test]
    fn satisfies_rejects_non_finite_inputs() {
        assert_eq!(
            satisfies(f64::NAN, 9.0, BoundKind::Point),
            None,
            "a NaN claim is not adjudicable"
        );
        assert_eq!(
            satisfies(10.0, f64::NAN, BoundKind::Upper),
            None,
            "a NaN observation is not adjudicable"
        );
        assert_eq!(
            satisfies(f64::INFINITY, 9.0, BoundKind::Upper),
            None,
            "an infinite claim admits no meaningful tolerance"
        );
    }

    #[test]
    fn tolerance_absorbs_round_trip_noise_but_not_distinct_integers() {
        assert_eq!(
            satisfies(9.0, 8.999_999_999_999_998, BoundKind::Point),
            Some(true),
            "a parse artefact must not surface as a mismatch"
        );
        assert_eq!(
            satisfies(1e6, 1e6 + 1.0, BoundKind::Point),
            Some(false),
            "adjacent large integers stay distinguishable"
        );
        assert_eq!(
            satisfies(1e-80, 1e-90, BoundKind::Point),
            Some(false),
            "a relative tolerance keeps very small magnitudes distinguishable"
        );
    }

    #[test]
    fn range_endpoints_are_normalised() {
        assert_eq!(
            range_endpoints("between 5 and 10 rows were removed"),
            Some((5.0, 10.0)),
            "explicit interval endpoints"
        );
        assert_eq!(
            range_endpoints("1,000 to 2,000 rows were removed"),
            Some((1000.0, 2000.0)),
            "thousands separators are stripped"
        );
        assert_eq!(
            range_endpoints("10 to 5 rows were removed"),
            Some((5.0, 10.0)),
            "endpoints are returned low-first regardless of textual order"
        );
        assert_eq!(
            range_endpoints("5–10 rows were removed"),
            Some((5.0, 10.0)),
            "en-dash interval"
        );
        assert_eq!(
            range_endpoints("fewer than 10 rows were removed"),
            None,
            "a bound is not an interval"
        );
    }

    #[test]
    fn range_containment_is_inclusive_and_order_insensitive() {
        let endpoints = range_endpoints("between 5 and 10 rows were removed")
            .expect("fixture states an interval");
        assert!(
            range_contains(endpoints, 5.0),
            "the lower endpoint is inside a closed interval"
        );
        assert!(
            range_contains(endpoints, 10.0),
            "the upper endpoint is inside a closed interval"
        );
        assert!(
            range_contains(endpoints, 7.0),
            "an interior quantity is inside"
        );
        assert!(
            !range_contains(endpoints, 4.0),
            "a quantity below the interval is outside"
        );
        assert!(
            !range_contains(endpoints, 11.0),
            "a quantity above the interval is outside"
        );
        assert!(
            range_contains((10.0, 5.0), 7.0),
            "an unnormalised pair still contains its interior"
        );
        assert!(
            !range_contains(endpoints, f64::NAN),
            "a NaN quantity is inside no interval"
        );
    }
}
