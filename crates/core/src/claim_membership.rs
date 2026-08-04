//! Closed-world membership check over the numerals a narrative asserts.
//!
//! Semantic claim verification asks an open-world question — *which computed
//! observable does this number refer to?* — and any binding it guesses can be
//! wrong, so a mis-binding convicts an honest narrative. Membership asks a
//! decidable question instead: *did the producing stage compute this number at
//! all?* A numeral that appears nowhere among the values the stage recorded, and
//! that falls into no declared exception class, cannot have come from the
//! computation; it was invented. Because the test is exact set membership plus a
//! fixed list of exception classes, the false-positive rate of this module is
//! bounded by the completeness of those classes rather than by the accuracy of
//! any inference.
//!
//! The module is modality-neutral by construction: it never inspects domain
//! terms, only numeral syntax and the citation / versioning / ordinal /
//! enumeration conventions of prose. It therefore applies unchanged to any
//! workflow class, including ones not yet implemented.
//!
//! Two pure functions:
//!
//! * [`extract_numerals`] — every numeral in prose, with its span and clause,
//!   skipping fenced blocks and inline code spans (which quote command lines and
//!   tabular fragments whose numerals assert nothing).
//! * [`classify`] — one numeral against the `(key, value)` pairs the stage
//!   recorded, yielding [`MembershipVerdict::Present`],
//!   [`MembershipVerdict::Exempt`] or [`MembershipVerdict::Absent`].
//!
//! Known scope limits, stated so callers do not over-trust the verdict:
//! numerals written in Unicode superscript / `×10ⁿ` form are not canonicalized
//! here (spans must stay valid against the raw narrative, and canonicalization
//! rewrites lengths); a decimal with no integer part (`.5`) is not extracted;
//! and the hyphen-collapsed exponent form (`10-80` meaning `1e-80`) parses as
//! two numerals.

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::LazyLock;

/// Relative slack added to every comparison so binary floating-point round-trip
/// error can never manufacture an `Absent` verdict for a value the stage did
/// record.
const REL_EPSILON: f64 = 1e-12;

/// Bytes of context retained on each side of a numeral when its clause is
/// captured. Bounds the reported clause and keeps cue detection local, so a cue
/// word from an unrelated sentence cannot exempt a numeral.
const CLAUSE_WINDOW: usize = 200;

/// Shortest digit run that an identifier cue earlier in the same clause may
/// exempt. Long bare integer runs after a cue such as `PMID` are catalogue
/// identifiers; shorter numerals in the same clause stay subject to membership,
/// so a cue word cannot launder every quantity around it.
const CUE_IDENTIFIER_MIN_DIGITS: usize = 6;

/// One numeral occurring in narrative prose, with the span it occupied.
///
/// The invariant this type carries: `literal` is exactly the narrative bytes at
/// `span`, and `clause` contains `literal`. Callers can therefore highlight the
/// numeral in the source narrative and classify it from `clause` alone. `value`
/// is the parsed magnitude with thousands separators removed and a trailing
/// percent sign dropped — `4.3%` yields `4.3`, never `0.043` — so a narrative's
/// own display convention is preserved for the reader while comparison handles
/// the scale.
#[derive(Debug, Clone, PartialEq)]
pub struct NarrativeNumeral {
    /// Parsed magnitude of the numeral.
    pub value: f64,
    /// Numeral text exactly as it appeared, including any thousands separators
    /// and a trailing `%`, so display precision stays recoverable.
    pub literal: String,
    /// Byte span of `literal` within the narrative passed to
    /// [`extract_numerals`].
    pub span: Range<usize>,
    /// The surrounding clause, for exception classification and reporting.
    pub clause: String,
}

/// Why a numeral needs no registry backing.
///
/// Each case names a prose convention that legitimately puts a number in text
/// that the producing stage never computed. Enumerating them is what keeps
/// [`MembershipVerdict::Absent`] a conviction rather than a guess: a numeral is
/// only called invented once every convention below has been excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ExemptionReason {
    /// A cutoff the declared policy puts in prose (`0.05`, `1.0`); it is an
    /// input to the computation, not an output of it.
    DeclaredThreshold,
    /// A calendar year in the policy window used as a citation year, or a
    /// component of an ISO calendar date.
    CalendarYear,
    /// A catalogue identifier: digits embedded in an alphanumeric token, or a
    /// long digit run introduced by an identifier cue.
    Identifier,
    /// A dotted or prefixed version designator (`1.2.3`, `v2`).
    VersionString,
    /// A pointer to a figure, table, panel, section or equation.
    FigureOrTableOrdinal,
    /// An ordinal position: a suffixed ordinal (`3rd`) or an enumeration marker
    /// opening a list item or heading.
    Ordinal,
    /// A cardinal at or below the policy's small-cardinal ceiling, where prose
    /// counting is indistinguishable from a computed count.
    SmallCardinal,
    /// A percentage that no recorded value backs at either scale; percentages
    /// are frequently rounded restatements rather than recorded quantities.
    Percentage,
    /// A bracketed reference marker, page, volume or chapter number.
    Citation,
}

/// The verdict for one numeral against one stage's recorded values.
///
/// `Absent` is the only failure and is reached last: it means the numeral
/// matched no recorded value at its own display precision and satisfied no
/// exemption. That is the fabrication signal this module exists to produce.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MembershipVerdict {
    /// The stage recorded this value. `keys` lists every registry key whose
    /// value matches, sorted for deterministic reporting.
    Present {
        /// Matching registry keys, ascending.
        keys: Vec<String>,
    },
    /// The numeral needs no backing; the case says which convention covers it.
    Exempt(ExemptionReason),
    /// No recorded value matches and no exemption applies.
    Absent,
}

/// Tunables for the exception classes, so a caller can widen or narrow the
/// exemptions without editing the classifier.
///
/// Every field exists to suppress a known class of false conviction: declared
/// cutoffs appear in prose by design, tiny cardinals are prose counting, and a
/// four-digit integer in the year window next to a citation cue is a date.
#[derive(Debug, Clone, PartialEq)]
pub struct MembershipPolicy {
    /// Thresholds the declared schemas legitimately put in prose (0.05, 1.0, …).
    pub declared_thresholds: Vec<f64>,
    /// Cardinals at or below this are treated as prose counting rather than
    /// computed observables ("the two retained inputs"). Default 3.
    pub small_cardinal_max: i64,
    /// Calendar-year window treated as a citation year, inclusive.
    pub year_range: (i64, i64),
}

impl Default for MembershipPolicy {
    /// Conservative defaults: no declared cutoffs assumed (the caller supplies
    /// what its schemas declare), prose counting up to three, and a calendar
    /// window wide enough for any citation year a narrative can carry.
    fn default() -> Self {
        Self {
            declared_thresholds: Vec::new(),
            small_cardinal_max: 3,
            year_range: (1800, 2200),
        }
    }
}

/// A numeral: optional sign, an integer run that may carry thousands
/// separators, an optional fraction, an optional decimal exponent, and an
/// optional trailing percent sign.
///
/// The grouped-thousands alternative precedes the plain-digit alternative so
/// `22,369` is taken whole; leftmost-longest would otherwise stop at `22` and
/// split one quantity into two, which membership would then judge separately
/// and convict twice.
static NUMERAL_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"[+-]?(?:[0-9]{1,3}(?:,[0-9]{3})+|[0-9]+)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?%?",
    )
    .expect("static regex")
});

/// Version cue immediately preceding a numeral (`v2`, `version 5`, `rev. 3`).
/// Anchored at the end so only adjacent text can exempt; a cue elsewhere in the
/// clause must not turn a recorded quantity into a version designator.
static VERSION_CUE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(?:v|ver|version|versions|rev|revision|release)[\s.:=#]*$")
        .expect("static regex")
});

/// Figure / table / section pointer cue immediately preceding a numeral.
/// Anchored, for the same reason as the version cue: a table caption mentioning
/// "Table" must not exempt the quantities inside its own prose.
static FIGURE_CUE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:figures?|figs?|tables?|panels?|plots?|charts?|sections?|subsections?|steps?|appendix|appendices|equations?|eqs?|listings?|notes?|items?|rows?|columns?|supplementary|supplement)[\s.:#()\[\]-]*$",
    )
    .expect("static regex")
});

/// Bibliographic locator cue immediately preceding a numeral (`pp. 12`,
/// `vol. 4`, `et al., 2019`).
static CITATION_CUE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(?:\bpp?\.|\bvols?\.?|\bnos?\.|\bchapters?|\bch\.|\bissue|et\s+al\.?,?)[\s:#]*$",
    )
    .expect("static regex")
});

/// Bibliographic-context cue anywhere in the clause, used only to admit a
/// four-digit year in the policy window. Purely bibliographic vocabulary — it
/// carries no subject-matter terms, so it generalizes across workflow classes.
static CITATION_CONTEXT_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:et\s+al|cite[ds]?|citation|citations|cited|reference[ds]?|references|publish(?:ed|ing)?|publication|preprint|journal|proceedings|bibliograph\w*|doi|pmid|pmcid|arxiv|biorxiv|medrxiv)\b",
    )
    .expect("static regex")
});

/// Identifier cue immediately preceding a numeral (`PMID:1234`, `DOI 10.1/x`,
/// `#42`, `id=7`).
static IDENTIFIER_CUE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(?:\bpmid|\bpmcid|\bdoi|\bids?|\baccessions?|\bidentifiers?|\bseeds?|\bhash|\bport|\buuid|#)[\s:=.#/-]*$",
    )
    .expect("static regex")
});

/// A colon-namespaced identifier: a bare word joined to digits by a colon with
/// no space (`data:3917`, `sha256:14`). Prose puts a space after a labelling
/// colon (`retained: 22369`), so a quantity is not caught by this.
static NAMESPACED_ID_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?:^|[^A-Za-z0-9_])[A-Za-z_][A-Za-z0-9_]*:$").expect("static regex")
});

/// Identifier cue anywhere in the clause, used only to admit long bare digit
/// runs (see [`CUE_IDENTIFIER_MIN_DIGITS`]), which is how comma-separated
/// catalogue lists appear once their cue has been consumed by the first entry.
static IDENTIFIER_CONTEXT_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(?:pmid|pmcid|pmids|doi|dois|accessions?|identifiers?)\b")
        .expect("static regex")
});

/// Ordinal suffix immediately following a numeral (`3rd`, `21st`).
static ORDINAL_SUFFIX_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)^(?:st|nd|rd|th)\b").expect("static regex"));

/// Only markdown structure may precede an enumeration marker: heading hashes,
/// list bullets, quote markers, table pipes.
static ENUMERATION_PREFIX_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[\s>*|#+-]*$").expect("static regex"));

/// An enumeration marker closes with `.` or `)` followed by space or end of
/// clause (`1. `, `10) `).
static ENUMERATION_SUFFIX_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[.)](?:\s|$)").expect("static regex"));

/// The numeral is followed by a table-field delimiter, so it occupies a whole
/// field rather than sitting inside prose.
static TABLE_FIELD_END_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\s*\|").expect("static regex"));

/// An upper-case acronym joined to digits by a hyphen (`SHA-256`, `UTF-8`,
/// `ISO-8601`). The hyphen keeps the digits out of the enclosing token, so
/// without this rule such designators reach the fabrication verdict. Restricted
/// to upper-case prefixes so a lower-case hyphenated compound (`top-500`) stays
/// subject to membership.
static ACRONYM_HYPHEN_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?:^|[^A-Za-z0-9])[A-Z][A-Z0-9]{1,9}-$").expect("static regex")
});

/// ISO calendar-date tail following a year (`-08`, `-08-04`). Combined with a
/// four-digit numeral, so a hyphenated numeric range is not taken for a date.
static ISO_DATE_TAIL_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^-[0-9]{2}(?:-[0-9]{2})?\b").expect("static regex"));

/// ISO calendar-date head preceding a month or day component. The four-digit
/// year must not itself be preceded by a digit, and the numeral it introduces
/// must be two digits, so `23124-25998` stays a range of quantities rather than
/// a date whose components would be exempt.
static ISO_DATE_HEAD_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?:^|[^0-9])[0-9]{4}-(?:[0-9]{2}-)?$").expect("static regex")
});

/// Extract every numeral from narrative prose, ignoring numerals inside fenced
/// code blocks and inline code spans.
///
/// The invariant: the returned spans index the input unchanged and appear in
/// ascending order, and no returned span overlaps a code region. Skipping code
/// is what keeps the check honest — narratives quote command lines and tabular
/// fragments dense with numerals that assert nothing, and treating those as
/// claims would swamp any real fabrication in noise.
///
/// A numeral whose text cannot be parsed as a finite `f64` is dropped rather
/// than reported, so an unparseable run never becomes a spurious conviction.
pub fn extract_numerals(narrative: &str) -> Vec<NarrativeNumeral> {
    let code = code_spans(narrative);
    let bytes = narrative.as_bytes();
    let mut out = Vec::new();
    for matched in NUMERAL_RE.find_iter(narrative) {
        let mut start = matched.start();
        let end = matched.end();
        // A leading sign belongs to the numeral only when nothing numeric or
        // alphanumeric abuts it. Otherwise the hyphen is a separator (an ISO
        // date, a hyphenated range) and swallowing it would invent a negative
        // quantity the narrative never wrote.
        if matches!(bytes[start], b'+' | b'-')
            && start > 0
            && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'.')
        {
            start += 1;
        }
        if start >= end || overlaps_code(&code, start, end) {
            continue;
        }
        let literal = &narrative[start..end];
        let Some(value) = parse_literal(literal) else {
            continue;
        };
        out.push(NarrativeNumeral {
            value,
            literal: literal.to_string(),
            span: start..end,
            clause: clause_of(narrative, start, end),
        });
    }
    out
}

/// Classify one numeral against the values a stage computed.
///
/// `registry` is every numeric value that stage recorded, as `(key, value)`
/// pairs; the key travels so a `Present` verdict can name what backed the
/// numeral instead of leaving the caller to re-derive it.
///
/// Order of adjudication, and why it is fixed: recorded membership first (a
/// number the stage computed is never merely "exempt"), then every exception
/// class, then — only if all of them fail — `Absent`. Comparison happens at the
/// numeral's own display precision, so an honestly rounded restatement of a
/// recorded value matches while a perturbed integer does not.
///
/// When the same literal occurs more than once inside one clause, contextual
/// rules are evaluated at every occurrence and an exemption fires if any
/// occurrence satisfies it. That bias is deliberate: this module's value rests
/// on `Absent` being trustworthy, so ambiguity resolves away from conviction.
pub fn classify(
    numeral: &NarrativeNumeral,
    registry: &[(String, f64)],
    policy: &MembershipPolicy,
) -> MembershipVerdict {
    if !numeral.value.is_finite() {
        return MembershipVerdict::Absent;
    }
    let tolerance = display_tolerance(&numeral.literal);
    let percent = is_percent_literal(&numeral.literal);

    let mut keys = BTreeSet::new();
    for (key, recorded) in registry {
        if values_match(*recorded, numeral.value, tolerance) {
            keys.insert(key.clone());
        } else if percent && values_match(*recorded, numeral.value / 100.0, tolerance / 100.0) {
            // A percentage is often the display form of a recorded fraction;
            // matching both scales prevents an honest restatement from being
            // written off as an unbacked percentage.
            keys.insert(key.clone());
        }
    }
    if !keys.is_empty() {
        return MembershipVerdict::Present {
            keys: keys.into_iter().collect(),
        };
    }

    if let Some(reason) = exemption(numeral, policy, tolerance, percent) {
        return MembershipVerdict::Exempt(reason);
    }
    MembershipVerdict::Absent
}

/// Local text on each side of one occurrence of a numeral inside its clause.
struct Context<'a> {
    before: &'a str,
    after: &'a str,
}

/// Adjudicate the exception classes in fixed order, returning the first that
/// fires. Order matters where classes overlap: a suffixed ordinal and a
/// version-prefixed numeral both look like digits glued to letters, so the
/// specific conventions are tested before the generic identifier rule.
fn exemption(
    numeral: &NarrativeNumeral,
    policy: &MembershipPolicy,
    tolerance: f64,
    percent: bool,
) -> Option<ExemptionReason> {
    if policy
        .declared_thresholds
        .iter()
        .any(|threshold| values_match(*threshold, numeral.value, tolerance))
    {
        return Some(ExemptionReason::DeclaredThreshold);
    }

    let contexts = contexts(&numeral.clause, &numeral.literal);
    let any = |predicate: &dyn Fn(&Context<'_>) -> bool| contexts.iter().any(|ctx| predicate(ctx));

    if any(&is_version) {
        return Some(ExemptionReason::VersionString);
    }
    if any(&|ctx: &Context<'_>| FIGURE_CUE_RE.is_match(ctx.before)) {
        return Some(ExemptionReason::FigureOrTableOrdinal);
    }
    if any(&is_ordinal) {
        return Some(ExemptionReason::Ordinal);
    }
    if any(&is_citation_locator) {
        return Some(ExemptionReason::Citation);
    }
    if any(&|ctx: &Context<'_>| is_calendar(ctx, numeral, policy)) {
        return Some(ExemptionReason::CalendarYear);
    }
    if any(&|ctx: &Context<'_>| is_identifier(ctx, &numeral.literal, &numeral.clause)) {
        return Some(ExemptionReason::Identifier);
    }
    if percent {
        return Some(ExemptionReason::Percentage);
    }
    if is_integral(numeral.value)
        && numeral.value >= 0.0
        && numeral.value <= policy.small_cardinal_max as f64
    {
        return Some(ExemptionReason::SmallCardinal);
    }
    None
}

/// A numeral is part of a version designator when a version cue abuts it, or
/// when it sits inside a dotted numeric run (`1.50.2`), which no single decimal
/// quantity can be.
fn is_version(ctx: &Context<'_>) -> bool {
    if VERSION_CUE_RE.is_match(ctx.before) {
        return true;
    }
    let dotted_after = ctx
        .after
        .strip_prefix('.')
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    let dotted_before = ctx
        .before
        .strip_suffix('.')
        .is_some_and(|head| head.chars().next_back().is_some_and(|c| c.is_ascii_digit()));
    dotted_after || dotted_before
}

/// An ordinal is a suffixed ordinal (`3rd`), an enumeration marker opening a
/// list item or heading (`10. `), or the leading field of a table row, where the
/// numeral names a position or row key rather than a computed quantity. In all
/// three shapes only document structure precedes the numeral.
fn is_ordinal(ctx: &Context<'_>) -> bool {
    if ORDINAL_SUFFIX_RE.is_match(ctx.after) {
        return true;
    }
    if !ENUMERATION_PREFIX_RE.is_match(ctx.before) {
        return false;
    }
    if ENUMERATION_SUFFIX_RE.is_match(ctx.after) {
        return true;
    }
    ctx.before.trim_end().ends_with('|') && TABLE_FIELD_END_RE.is_match(ctx.after)
}

/// A bracketed reference marker (`[12]`) or a bibliographic locator cue
/// (`pp. 12`) points at a source, not at a computation.
fn is_citation_locator(ctx: &Context<'_>) -> bool {
    if ctx.before.ends_with('[') && ctx.after.starts_with(']') {
        return true;
    }
    CITATION_CUE_RE.is_match(ctx.before)
}

/// A bare four-digit integer inside the policy window counts as a year when it
/// is parenthesized or the clause carries bibliographic context; any component
/// of an ISO calendar date counts regardless of the window, since months and
/// days fall outside it.
fn is_calendar(ctx: &Context<'_>, numeral: &NarrativeNumeral, policy: &MembershipPolicy) -> bool {
    let digits = numeral.literal.len();
    if is_plain_integer(&numeral.literal) {
        // `YYYY-MM(-DD)`: a four-digit numeral opening the date, or a two-digit
        // component following one. The digit counts are what stop a hyphenated
        // range of quantities from being taken for a date.
        if digits == 4 && ISO_DATE_TAIL_RE.is_match(ctx.after) {
            return true;
        }
        if digits == 2 && ISO_DATE_HEAD_RE.is_match(ctx.before) {
            return true;
        }
    }
    if !is_plain_integer(&numeral.literal) || !is_integral(numeral.value) {
        return false;
    }
    let magnitude = numeral.value as i64;
    if magnitude < policy.year_range.0 || magnitude > policy.year_range.1 {
        return false;
    }
    let parenthesized = ctx.before.ends_with('(') && ctx.after.starts_with(')');
    parenthesized || CITATION_CONTEXT_RE.is_match(&numeral.clause)
}

/// A numeral is an identifier when digits are embedded in an alphanumeric token
/// (`ENS0001234`, `log2`), when an identifier cue abuts it (`PMID:1234`), or
/// when the clause carries identifier context and the numeral is a long bare
/// digit run — the shape of a catalogue list whose cue was consumed by its first
/// entry.
fn is_identifier(ctx: &Context<'_>, literal: &str, clause: &str) -> bool {
    // A brace-delimited numeral is a repetition count inside a pattern literal
    // (`\d{11}`), not a quantity. The delimiters must hug the numeral, so a
    // mapping literal (`{"a": 4}`) keeps its values checkable.
    if ctx.before.ends_with('{') && ctx.after.starts_with('}') {
        return true;
    }
    if token_remainder_has_letter(ctx)
        || IDENTIFIER_CUE_RE.is_match(ctx.before)
        || ACRONYM_HYPHEN_RE.is_match(ctx.before)
        || NAMESPACED_ID_RE.is_match(ctx.before)
    {
        return true;
    }
    let digits = literal.chars().filter(char::is_ascii_digit).count();
    is_plain_integer(literal)
        && digits >= CUE_IDENTIFIER_MIN_DIGITS
        && IDENTIFIER_CONTEXT_RE.is_match(clause)
}

/// True when the word-ish token enclosing the numeral carries letters outside
/// the numeral itself. Separators (`=`, `:`, `(`, `,`, whitespace, `-`) end the
/// token, so `n=22369` stays a quantity while `ENS22369` is an identifier.
fn token_remainder_has_letter(ctx: &Context<'_>) -> bool {
    let token_char = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '%';
    let head_letters = ctx
        .before
        .chars()
        .rev()
        .take_while(|c| token_char(*c))
        .any(|c| c.is_ascii_alphabetic() || c == '_');
    let tail_letters = ctx
        .after
        .chars()
        .take_while(|c| token_char(*c))
        .any(|c| c.is_ascii_alphabetic() || c == '_');
    head_letters || tail_letters
}

/// Locate every occurrence of the literal inside its clause and expose the text
/// on each side. When the literal is absent — a hand-built numeral whose clause
/// was not captured by [`extract_numerals`] — a single empty context is
/// returned, degrading classification to the value-only rules rather than
/// panicking or silently exempting.
fn contexts<'a>(clause: &'a str, literal: &str) -> Vec<Context<'a>> {
    if literal.is_empty() {
        return vec![Context {
            before: "",
            after: "",
        }];
    }
    let found: Vec<Context<'a>> = clause
        .match_indices(literal)
        .map(|(at, _)| Context {
            before: &clause[..at],
            after: &clause[at + literal.len()..],
        })
        .collect();
    if found.is_empty() {
        return vec![Context {
            before: "",
            after: "",
        }];
    }
    found
}

/// Parse a numeral literal, dropping thousands separators and a trailing
/// percent sign. Returns `None` for anything that is not a finite number, so
/// malformed runs are discarded instead of being convicted.
fn parse_literal(literal: &str) -> Option<f64> {
    let cleaned: String = literal.chars().filter(|c| *c != ',' && *c != '%').collect();
    cleaned.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Half the unit of the numeral's least significant digit — the widest gap that
/// honest rounding can open between a recorded value and its written form.
///
/// This is the integer-safe tolerance: an integer literal has unit 1 and
/// tolerance 0.5, so `22368` cannot match a recorded `22369`, while `4.16e-134`
/// tolerates only 5e-137. Fixed absolute or relative epsilons fail at one end
/// or the other of that range.
fn display_tolerance(literal: &str) -> f64 {
    let cleaned: String = literal
        .chars()
        .filter(|c| {
            c.is_ascii_digit() || *c == '.' || *c == 'e' || *c == 'E' || *c == '+' || *c == '-'
        })
        .collect();
    let body = cleaned.trim_start_matches(['+', '-']);
    let (mantissa, exponent) = match body.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => (body, 0),
    };
    let fraction_digits = mantissa
        .split_once('.')
        .map_or(0, |(_, frac)| frac.len().min(64) as i32);
    let half = 0.5 * 10f64.powi(exponent.saturating_sub(fraction_digits).clamp(-300, 300));
    if half.is_finite() {
        half
    } else {
        0.0
    }
}

/// Compare a recorded value against a claimed one at the claim's display
/// precision, widened by a relative epsilon so float round-trip noise never
/// convicts.
fn values_match(recorded: f64, claimed: f64, tolerance: f64) -> bool {
    if !recorded.is_finite() || !claimed.is_finite() {
        return false;
    }
    let slack = tolerance.max(claimed.abs() * REL_EPSILON);
    (recorded - claimed).abs() <= slack
}

/// True when the literal carries a trailing percent sign.
fn is_percent_literal(literal: &str) -> bool {
    literal.ends_with('%')
}

/// True when the literal is digits only — no sign, separator, fraction,
/// exponent or percent — the shape required of years and catalogue identifiers.
fn is_plain_integer(literal: &str) -> bool {
    !literal.is_empty() && literal.chars().all(|c| c.is_ascii_digit())
}

/// True when the value has no fractional part.
fn is_integral(value: f64) -> bool {
    value.fract() == 0.0
}

/// Byte ranges covering fenced code blocks and inline code spans, ascending and
/// non-overlapping.
///
/// Fences are toggled by a run of at least three backticks or tildes at up to
/// three columns of indent, and the closing run must be at least as long as the
/// opening one; the fence lines themselves are covered. Outside fences, backtick
/// runs pair with the next run of equal length, so a multi-backtick span
/// containing single backticks is handled as one region.
fn code_spans(narrative: &str) -> Vec<Range<usize>> {
    let mut spans: Vec<Range<usize>> = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut offset = 0usize;
    for line in narrative.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let run = leading_fence_run(trimmed);
        match fence {
            None => match run {
                Some((delimiter, length)) if length >= 3 && indent <= 3 => {
                    fence = Some((delimiter, length));
                    spans.push(offset..offset + line.len());
                }
                _ => spans.extend(inline_code_spans(line, offset)),
            },
            Some((open_delimiter, open_length)) => {
                spans.push(offset..offset + line.len());
                if let Some((delimiter, length)) = run {
                    if delimiter == open_delimiter && length >= open_length {
                        fence = None;
                    }
                }
            }
        }
        offset += line.len();
    }
    spans
}

/// Length of a leading run of backticks or tildes, if the line opens with one.
fn leading_fence_run(trimmed: &str) -> Option<(u8, usize)> {
    let bytes = trimmed.as_bytes();
    let first = *bytes.first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let length = bytes.iter().take_while(|b| **b == first).count();
    Some((first, length))
}

/// Inline code spans within one line, offset into the narrative.
fn inline_code_spans(line: &str, base: usize) -> Vec<Range<usize>> {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] != b'`' {
            cursor += 1;
            continue;
        }
        let open_end = cursor + bytes[cursor..].iter().take_while(|b| **b == b'`').count();
        let open_length = open_end - cursor;
        let mut scan = open_end;
        let mut close_end = None;
        while scan < bytes.len() {
            if bytes[scan] != b'`' {
                scan += 1;
                continue;
            }
            let run_end = scan + bytes[scan..].iter().take_while(|b| **b == b'`').count();
            if run_end - scan == open_length {
                close_end = Some(run_end);
                break;
            }
            scan = run_end;
        }
        match close_end {
            Some(end) => {
                spans.push(base + cursor..base + end);
                cursor = end;
            }
            None => cursor = open_end,
        }
    }
    spans
}

/// True when `[start, end)` intersects any code region.
fn overlaps_code(code: &[Range<usize>], start: usize, end: usize) -> bool {
    code.iter().any(|span| start < span.end && span.start < end)
}

/// Abbreviations whose trailing period must not be taken for a sentence end.
///
/// Every entry is a bibliographic, typographic or versioning abbreviation that
/// introduces a numeral (`pp. 12`, `Fig. 3`, `v. 2`). Splitting the clause at
/// such a period would strand the numeral from the cue that exempts it, turning
/// an ordinary reference into a fabrication verdict. Sorted for binary search
/// and for deterministic review.
const PROSE_ABBREVIATIONS: &[&str] = &[
    "al", "approx", "ca", "cf", "ch", "chap", "chaps", "ed", "eds", "eg", "eq", "eqs", "et", "fig",
    "figs", "ie", "no", "nos", "p", "pp", "ref", "refs", "rev", "sec", "secs", "tbl", "v", "ver",
    "vol", "vols", "vs",
];

/// True when the period at `dot` closes one of [`PROSE_ABBREVIATIONS`].
fn closes_abbreviation(bytes: &[u8], dot: usize) -> bool {
    let mut start = dot;
    while start > 0 && bytes[start - 1].is_ascii_alphabetic() && dot - start < 8 {
        start -= 1;
    }
    if start == dot {
        return false;
    }
    let word: String = bytes[start..dot]
        .iter()
        .map(|b| b.to_ascii_lowercase() as char)
        .collect();
    PROSE_ABBREVIATIONS.binary_search(&word.as_str()).is_ok()
}

/// True when the byte at `at` ends the clause: a line break, a semicolon, or a
/// sentence terminator followed by whitespace.
///
/// A `.` is a terminator only when the character before it is not a digit and
/// the word before it is not a prose abbreviation, so neither a decimal
/// quantity, an enumeration marker, nor a reference locator splits the clause
/// and strands the numeral from the cue that explains it.
fn terminates_clause(bytes: &[u8], at: usize, limit: usize) -> bool {
    let byte = bytes[at];
    if byte == b'\n' || byte == b';' {
        return true;
    }
    if !matches!(byte, b'.' | b'!' | b'?') {
        return false;
    }
    if at + 1 >= limit || !bytes[at + 1].is_ascii_whitespace() {
        return false;
    }
    if byte != b'.' {
        return true;
    }
    at > 0 && !bytes[at - 1].is_ascii_digit() && !closes_abbreviation(bytes, at)
}

/// Capture the clause enclosing a numeral: back to the previous line break,
/// semicolon or sentence terminator, forward to the next one, bounded by
/// [`CLAUSE_WINDOW`] bytes on each side.
fn clause_of(narrative: &str, start: usize, end: usize) -> String {
    let bytes = narrative.as_bytes();
    let floor = floor_boundary(narrative, start.saturating_sub(CLAUSE_WINDOW));
    let mut left = floor;
    let mut index = start;
    while index > floor {
        index -= 1;
        if terminates_clause(bytes, index, start) {
            left = index + 1;
            break;
        }
    }

    let ceiling = ceil_boundary(narrative, (end + CLAUSE_WINDOW).min(narrative.len()));
    let mut right = ceiling;
    let mut cursor = end;
    while cursor < ceiling {
        if terminates_clause(bytes, cursor, narrative.len()) {
            right = cursor;
            break;
        }
        cursor += 1;
    }

    let left = floor_boundary(narrative, left.min(start));
    let right = ceil_boundary(narrative, right.max(end));
    narrative[left..right]
        .trim_matches(|c: char| c.is_ascii_whitespace())
        .to_string()
}

/// Round an index down to the nearest character boundary.
fn floor_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Round an index up to the nearest character boundary.
fn ceil_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(pairs: &[(&str, f64)]) -> Vec<(String, f64)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), *value))
            .collect()
    }

    fn only(narrative: &str) -> NarrativeNumeral {
        let numerals = extract_numerals(narrative);
        assert_eq!(
            numerals.len(),
            1,
            "expected one numeral in {narrative:?}: {numerals:?}"
        );
        numerals.into_iter().next().expect("one numeral")
    }

    fn verdict(narrative: &str, pairs: &[(&str, f64)]) -> MembershipVerdict {
        classify(
            &only(narrative),
            &registry(pairs),
            &MembershipPolicy::default(),
        )
    }

    #[test]
    fn spans_and_clause_are_consistent() {
        let narrative = "The retained population is 22,369 units.";
        let numeral = only(narrative);
        assert_eq!(&narrative[numeral.span.clone()], numeral.literal);
        assert!(numeral.clause.contains(&numeral.literal));
        assert_eq!(numeral.value, 22369.0);
    }

    #[test]
    fn thousands_separators_parse_as_one_numeral() {
        let numeral = only("Retained: 1,234,567 items.");
        assert_eq!(numeral.literal, "1,234,567");
        assert_eq!(numeral.value, 1_234_567.0);
        assert_eq!(
            classify(
                &numeral,
                &registry(&[("n_retained", 1_234_567.0)]),
                &MembershipPolicy::default()
            ),
            MembershipVerdict::Present {
                keys: vec!["n_retained".to_string()]
            }
        );
    }

    #[test]
    fn scientific_notation_parses_and_matches() {
        let numeral = only("The smallest adjusted statistic is 4.16e-134 overall.");
        assert_eq!(numeral.value, 4.16e-134);
        assert_eq!(
            classify(
                &numeral,
                &registry(&[("min_statistic", 4.16e-134)]),
                &MembershipPolicy::default()
            ),
            MembershipVerdict::Present {
                keys: vec!["min_statistic".to_string()]
            }
        );

        let upper = only("Scaling factor 1.2E+3 applied.");
        assert_eq!(upper.value, 1200.0);
    }

    #[test]
    fn display_precision_admits_rounding_but_not_perturbation() {
        // Recorded 4.312345, written 4.312 — honest rounding, still Present.
        assert_eq!(
            verdict("Effect 4.312 recorded.", &[("effect", 4.312_345)]),
            MembershipVerdict::Present {
                keys: vec!["effect".to_string()]
            }
        );
        // Recorded 22369, written 22368 — off by one, and integers admit no
        // rounding slack, so the perturbation is convicted.
        assert_eq!(
            verdict("Retained 22368 units.", &[("n_retained", 22369.0)]),
            MembershipVerdict::Absent
        );
    }

    #[test]
    fn absent_is_reachable_for_an_invented_quantity() {
        let numeral = only("A total of 987654 units were retained.");
        assert_eq!(
            classify(
                &numeral,
                &registry(&[("n_retained", 22369.0)]),
                &MembershipPolicy::default()
            ),
            MembershipVerdict::Absent
        );
    }

    #[test]
    fn present_keys_are_sorted_and_deduplicated() {
        let numeral = only("Retained 8 units.");
        let pairs = registry(&[("z_count", 8.0), ("a_count", 8.0), ("z_count", 8.0)]);
        assert_eq!(
            classify(&numeral, &pairs, &MembershipPolicy::default()),
            MembershipVerdict::Present {
                keys: vec!["a_count".to_string(), "z_count".to_string()]
            }
        );
    }

    #[test]
    fn fenced_block_numerals_are_never_extracted() {
        let narrative = concat!(
            "Retained 22369 units.\n",
            "```\n",
            "threshold = 99999\n",
            "count = 12345\n",
            "```\n",
            "No further quantities.\n",
        );
        let numerals = extract_numerals(narrative);
        let literals: Vec<&str> = numerals.iter().map(|n| n.literal.as_str()).collect();
        assert_eq!(literals, vec!["22369"]);
        // The invented-looking numerals inside the fence cannot be convicted,
        // because they are never surfaced as claims at all.
        for numeral in &numerals {
            assert_ne!(numeral.literal, "99999");
        }
    }

    #[test]
    fn tilde_fences_and_long_fences_toggle() {
        let narrative = "A 5 here.\n~~~~\n88888\n~~~~\nB 6 here.\n";
        let literals: Vec<String> = extract_numerals(narrative)
            .into_iter()
            .map(|n| n.literal)
            .collect();
        assert_eq!(literals, vec!["5".to_string(), "6".to_string()]);
    }

    #[test]
    fn inline_code_spans_are_skipped() {
        let narrative = "Threshold `padj < 0.05` applied to 4025 units.";
        let literals: Vec<String> = extract_numerals(narrative)
            .into_iter()
            .map(|n| n.literal)
            .collect();
        assert_eq!(literals, vec!["4025".to_string()]);
    }

    #[test]
    fn multi_backtick_inline_span_is_skipped() {
        let narrative = "See ``a ` b 777`` and 42 units.";
        let literals: Vec<String> = extract_numerals(narrative)
            .into_iter()
            .map(|n| n.literal)
            .collect();
        assert_eq!(literals, vec!["42".to_string()]);
    }

    #[test]
    fn declared_threshold_is_exempt() {
        let numeral = only("Significance was assessed at 0.05 for every entry.");
        let policy = MembershipPolicy {
            declared_thresholds: vec![0.05],
            ..MembershipPolicy::default()
        };
        assert_eq!(
            classify(&numeral, &registry(&[("n_total", 22369.0)]), &policy),
            MembershipVerdict::Exempt(ExemptionReason::DeclaredThreshold)
        );
    }

    #[test]
    fn calendar_year_is_exempt_with_bibliographic_context() {
        let cited = only("As published in 1998, the approach predates this run.");
        assert_eq!(
            classify(&cited, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::CalendarYear)
        );
        let parenthesized = only("The prior report (2019) describes the protocol.");
        assert_eq!(
            classify(&parenthesized, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::CalendarYear)
        );
    }

    #[test]
    fn bare_four_digit_quantity_without_citation_context_stays_checkable() {
        // A year-shaped integer with no bibliographic cue is a quantity, so the
        // exemption must not swallow it.
        assert_eq!(
            verdict("Retained 1998 units.", &[("n_retained", 22369.0)]),
            MembershipVerdict::Absent
        );
    }

    #[test]
    fn iso_date_components_are_exempt() {
        let narrative = "Run stamped 2026-08-04 by the orchestrator.";
        let numerals = extract_numerals(narrative);
        assert_eq!(numerals.len(), 3, "{numerals:?}");
        for numeral in &numerals {
            assert_eq!(
                classify(numeral, &[], &MembershipPolicy::default()),
                MembershipVerdict::Exempt(ExemptionReason::CalendarYear),
                "{numeral:?}"
            );
        }
    }

    #[test]
    fn hyphenated_numeric_range_is_not_a_date() {
        // `23124-25998` shares the ISO date's punctuation but not its digit
        // counts; both endpoints must stay subject to membership.
        let numerals = extract_numerals("Recomputed span (23124-25998 of the population).");
        assert_eq!(numerals.len(), 2, "{numerals:?}");
        for numeral in &numerals {
            assert_eq!(
                classify(numeral, &[], &MembershipPolicy::default()),
                MembershipVerdict::Absent,
                "{numeral:?}"
            );
        }
    }

    #[test]
    fn identifier_forms_are_exempt() {
        let embedded = only("Record ABC00000152583 heads the ordering.");
        assert_eq!(
            classify(&embedded, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::Identifier)
        );

        let cued = only("Concordant against PMID 35902923 in the retained snapshot.");
        assert_eq!(
            classify(&cued, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::Identifier)
        );

        // Comma-separated catalogue list: the cue introduces the run, so the
        // later long integers are exempt too.
        let listed = extract_numerals("Retrieved identifiers: 18178867, 24926665, 26207385.");
        assert_eq!(listed.len(), 3);
        for numeral in &listed {
            assert_eq!(
                classify(numeral, &[], &MembershipPolicy::default()),
                MembershipVerdict::Exempt(ExemptionReason::Identifier),
                "{numeral:?}"
            );
        }
    }

    #[test]
    fn identifier_cue_does_not_launder_neighbouring_quantities() {
        // The clause carries an identifier cue, but the quantity is not a long
        // bare integer, so membership still applies to it.
        let numerals =
            extract_numerals("Concordant against PMID 35902923 with effect 9.999 recorded.");
        let effect = numerals
            .iter()
            .find(|n| n.literal == "9.999")
            .expect("effect numeral");
        assert_eq!(
            classify(
                effect,
                &registry(&[("effect", 1.467)]),
                &MembershipPolicy::default()
            ),
            MembershipVerdict::Absent
        );
    }

    #[test]
    fn assignment_prefix_is_not_an_identifier() {
        // `n=` must not count as an alphanumeric identifier token, or every
        // labelled quantity would be exempt.
        assert_eq!(
            verdict("Population n=987654 supplied.", &[("n_total", 22369.0)]),
            MembershipVerdict::Absent
        );
    }

    #[test]
    fn version_strings_are_exempt() {
        let dotted = extract_numerals("Executed under runtime 4.5.3 as recorded.");
        assert_eq!(dotted.len(), 2, "{dotted:?}");
        for numeral in &dotted {
            assert_eq!(
                classify(numeral, &[], &MembershipPolicy::default()),
                MembershipVerdict::Exempt(ExemptionReason::VersionString),
                "{numeral:?}"
            );
        }

        let prefixed = only("Schema v7 governs the record.");
        assert_eq!(
            classify(&prefixed, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::VersionString)
        );

        let cued = only("Toolchain version 19 was pinned.");
        assert_eq!(
            classify(&cued, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::VersionString)
        );
    }

    #[test]
    fn figure_and_table_ordinals_are_exempt() {
        for narrative in [
            "Figure 7 shows the distribution.",
            "Table 12 lists the retained entries.",
            "Panel 9 repeats the comparison.",
            "See Section 42 for the derivation.",
        ] {
            let numeral = only(narrative);
            assert_eq!(
                classify(&numeral, &[], &MembershipPolicy::default()),
                MembershipVerdict::Exempt(ExemptionReason::FigureOrTableOrdinal),
                "{narrative}"
            );
        }
    }

    #[test]
    fn table_caption_does_not_exempt_its_own_quantities() {
        // The cue is not adjacent, so the quantity in the caption prose stays
        // subject to membership.
        let numerals = extract_numerals("Table drawn from a pool of 987654 eligible entries.");
        let pool = numerals
            .iter()
            .find(|n| n.literal == "987654")
            .expect("pool numeral");
        assert_eq!(
            classify(
                pool,
                &registry(&[("pool", 2209.0)]),
                &MembershipPolicy::default()
            ),
            MembershipVerdict::Absent
        );
    }

    #[test]
    fn ordinals_are_exempt() {
        let suffixed = only("The 47th position was retained.");
        assert_eq!(
            classify(&suffixed, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::Ordinal)
        );

        let enumerated = only("## 12. Provenance of the retained inputs");
        assert_eq!(
            classify(&enumerated, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::Ordinal)
        );

        let listed = only("10) Restate the retained population");
        assert_eq!(
            classify(&listed, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::Ordinal)
        );
    }

    #[test]
    fn table_row_key_field_is_an_ordinal() {
        // Only structure precedes the numeral and a field delimiter follows, so
        // it is the row key, not an observable the stage computed.
        let numerals = extract_numerals("| 17 | rule_name | PASS | 987654 entries |\n");
        let key = numerals
            .iter()
            .find(|n| n.literal == "17")
            .expect("row key");
        assert_eq!(
            classify(key, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::Ordinal)
        );
        // A quantity in a later field is still checked: prose precedes it.
        let quantity = numerals
            .iter()
            .find(|n| n.literal == "987654")
            .expect("quantity");
        assert_eq!(
            classify(quantity, &[], &MembershipPolicy::default()),
            MembershipVerdict::Absent
        );
    }

    #[test]
    fn pattern_repetition_count_is_exempt_but_mapping_values_are_not() {
        let quantifier = only(r"Identifiers match ^ABC\d{11}$ exactly.");
        assert_eq!(
            classify(&quantifier, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::Identifier)
        );
        let mapped = extract_numerals("Recomputed level counts {'a': 987654, 'b': 4}.");
        let value = mapped
            .iter()
            .find(|n| n.literal == "987654")
            .expect("mapped value");
        assert_eq!(
            classify(value, &[], &MembershipPolicy::default()),
            MembershipVerdict::Absent
        );
    }

    #[test]
    fn colon_namespaced_identifier_is_exempt_but_a_labelled_quantity_is_not() {
        let namespaced = only("The declared port is data:3917 for that transition.");
        assert_eq!(
            classify(&namespaced, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::Identifier)
        );
        // A labelling colon carries a space, so the quantity stays checkable.
        assert_eq!(
            verdict("Retained: 987654 units", &[("n", 22369.0)]),
            MembershipVerdict::Absent
        );
    }

    #[test]
    fn hyphenated_acronym_designator_is_an_identifier() {
        let hashed = only("Bytes were re-hashed with SHA-256 and compared.");
        assert_eq!(
            classify(&hashed, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::Identifier)
        );
        // A lower-case hyphenated compound is prose, so its numeral stays
        // subject to membership.
        let ranked = only("The top-987654 ordering was retained.");
        assert_eq!(
            classify(&ranked, &[], &MembershipPolicy::default()),
            MembershipVerdict::Absent
        );
    }

    #[test]
    fn small_cardinals_are_exempt() {
        let numeral = only("Both of the 2 retained inputs were used.");
        assert_eq!(
            classify(&numeral, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::SmallCardinal)
        );
        // Above the ceiling the cardinal is a computed count again.
        let bigger = only("All 9 retained inputs were used.");
        assert_eq!(
            classify(&bigger, &[], &MembershipPolicy::default()),
            MembershipVerdict::Absent
        );
    }

    #[test]
    fn percentages_are_exempt_only_when_unbacked() {
        let unbacked = only("Roughly 4.3% of the ordering was tied.");
        assert_eq!(
            classify(
                &unbacked,
                &registry(&[("n_total", 22369.0)]),
                &MembershipPolicy::default()
            ),
            MembershipVerdict::Exempt(ExemptionReason::Percentage)
        );

        // Recorded as a fraction, restated as a percentage: Present, not exempt.
        let backed = only("Roughly 4.3% of the ordering was tied.");
        assert_eq!(
            classify(
                &backed,
                &registry(&[("tied_fraction", 0.043)]),
                &MembershipPolicy::default()
            ),
            MembershipVerdict::Present {
                keys: vec!["tied_fraction".to_string()]
            }
        );

        // Recorded at percentage scale: also Present.
        let direct = only("Roughly 4.3% of the ordering was tied.");
        assert_eq!(
            classify(
                &direct,
                &registry(&[("tied_percent", 4.3)]),
                &MembershipPolicy::default()
            ),
            MembershipVerdict::Present {
                keys: vec!["tied_percent".to_string()]
            }
        );
    }

    #[test]
    fn citation_markers_are_exempt() {
        let bracketed = only("The approach follows the prior protocol [17].");
        assert_eq!(
            classify(&bracketed, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::Citation)
        );

        let locator = only("Described at pp. 88 of that source.");
        assert_eq!(
            classify(&locator, &[], &MembershipPolicy::default()),
            MembershipVerdict::Exempt(ExemptionReason::Citation)
        );
    }

    #[test]
    fn negative_quantities_keep_their_sign() {
        let numeral = only("Effect -3.449 recorded for that entry.");
        assert_eq!(numeral.value, -3.449);
        assert_eq!(
            classify(
                &numeral,
                &registry(&[("effect", -3.449)]),
                &MembershipPolicy::default()
            ),
            MembershipVerdict::Present {
                keys: vec!["effect".to_string()]
            }
        );
    }

    #[test]
    fn hyphenated_range_does_not_invent_a_negative() {
        let numerals = extract_numerals("Window 40-60 units wide.");
        let values: Vec<f64> = numerals.iter().map(|n| n.value).collect();
        assert_eq!(values, vec![40.0, 60.0]);
    }

    #[test]
    fn generalizes_to_a_non_scientific_narrative() {
        // A logistics narrative: no subject-matter vocabulary is involved in any
        // rule, so the same classifier applies unchanged.
        let narrative = concat!(
            "## 3. Fleet summary\n",
            "Dispatched 1,204 shipments against a fleet of 9 vehicles, ",
            "logged under manifest MF00042199 at scheduler version 2.4.1.\n",
            "Late arrivals: 87 shipments, or 7.2% of the dispatched total ",
            "(see Table 4). The historical baseline was published in 2011 [3].\n",
            "Median transit time was 5100 minutes.\n",
        );
        let recorded = registry(&[
            ("shipments_dispatched", 1204.0),
            ("fleet_size", 9.0),
            ("late_arrivals", 87.0),
            ("late_fraction", 0.0723),
            ("median_transit_minutes", 4210.0),
        ]);
        let policy = MembershipPolicy::default();

        let mut convicted = Vec::new();
        let mut backed = Vec::new();
        for numeral in extract_numerals(narrative) {
            match classify(&numeral, &recorded, &policy) {
                MembershipVerdict::Absent => convicted.push(numeral.literal),
                MembershipVerdict::Present { .. } => backed.push(numeral.literal),
                MembershipVerdict::Exempt(_) => {}
            }
        }
        // Only the invented transit time is convicted; the manifest identifier,
        // the version designator, the heading ordinal, the table pointer, the
        // citation year and the bracketed marker are all exempt.
        assert_eq!(
            convicted,
            vec!["5100".to_string()],
            "unexpected convictions"
        );
        assert!(backed.contains(&"1,204".to_string()));
        assert!(backed.contains(&"87".to_string()));
        assert!(
            backed.contains(&"7.2%".to_string()),
            "percentage should bind to the recorded fraction"
        );
    }

    #[test]
    fn code_only_numerals_never_convict_in_a_mixed_narrative() {
        let narrative = concat!(
            "Applied the documented rule to 22369 entries.\n",
            "\n",
            "```text\n",
            "row_sum >= 10\n",
            "invented = 424242\n",
            "```\n",
            "Inline restatement: `invented = 424242`.\n",
        );
        let recorded = registry(&[("n_retained", 22369.0)]);
        let convicted: Vec<String> = extract_numerals(narrative)
            .into_iter()
            .filter(|numeral| {
                classify(numeral, &recorded, &MembershipPolicy::default())
                    == MembershipVerdict::Absent
            })
            .map(|numeral| numeral.literal)
            .collect();
        assert!(
            convicted.is_empty(),
            "code regions must not convict: {convicted:?}"
        );
    }

    #[test]
    fn clause_capture_is_bounded_and_char_safe() {
        let narrative = format!("{} 987654 {}", "é".repeat(400), "ü".repeat(400));
        let numeral = only(&narrative);
        assert!(numeral.clause.contains("987654"));
        assert!(numeral.clause.len() <= 987654_usize.to_string().len() + 2 * CLAUSE_WINDOW + 2);
    }

    #[test]
    fn abbreviation_table_is_sorted_for_binary_search() {
        let mut sorted = PROSE_ABBREVIATIONS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, PROSE_ABBREVIATIONS.to_vec());
    }

    #[test]
    fn unparseable_and_hand_built_numerals_degrade_safely() {
        // A hand-built numeral whose clause does not contain the literal falls
        // back to value-only rules rather than panicking.
        let numeral = NarrativeNumeral {
            value: 987654.0,
            literal: "987654".to_string(),
            span: 0..6,
            clause: "clause without the literal".to_string(),
        };
        assert_eq!(
            classify(&numeral, &[], &MembershipPolicy::default()),
            MembershipVerdict::Absent
        );

        let nan = NarrativeNumeral {
            value: f64::NAN,
            literal: "nan".to_string(),
            span: 0..3,
            clause: "nan".to_string(),
        };
        assert_eq!(
            classify(&nan, &[], &MembershipPolicy::default()),
            MembershipVerdict::Absent
        );
    }
}
