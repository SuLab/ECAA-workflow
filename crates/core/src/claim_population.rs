//! Population-granularity admissibility of a narrative-count → observed-field
//! binding.
//!
//! A verifier that binds a numeral in prose to a scalar field of a
//! machine-emitted summary can be wrong in two independent ways. The binding
//! can be *dimensionally* wrong — a cardinality bound to an extremum, which
//! [`crate::claim_quantity`] rejects — or it can be dimensionally sound and
//! still wrong about *what is being counted*: two genuine cardinalities over
//! two different populations. A count of the entries of a rectangular matrix
//! and a count of that matrix's retained rows are both cardinalities, so no
//! dimensional filter can separate them; binding one to the other and
//! reporting the numeric difference convicts a narrative that was correct.
//!
//! The separating datum is the **declared** population — the
//! `population` field of a reporting contract's `Observable`, which is the
//! stage's own answer to "a count of *what*". This module is the discriminator
//! over that declaration, and it is deliberately not an inference engine: it
//! never derives a population from a field name, from a value, or from a table
//! of nouns. Every population term it compares comes either from the
//! declaration or from the narrative clause at run time, which is what lets it
//! adjudicate every workflow modality — including modalities that do not exist
//! yet — with no term belonging to any one field of study appearing in this
//! source at all.
//!
//! Three verdicts, and only one of them may support a finding:
//!
//! * [`PopulationAgreement::Agrees`] — the clause's counted subject is a
//!   surface form the declaration accepts. Like the dimensional filter's
//!   admissibility, this is the *absence of an objection*, never evidence for
//!   the binding.
//! * [`PopulationAgreement::Disagrees`] — the declaration and the clause both
//!   identify a population and they are different populations. The only
//!   verdict [`may_convict`] admits.
//! * [`PopulationAgreement::Undeclared`] — nothing was declared, or the clause's
//!   subject cannot be identified, or the declaration is silent on the question
//!   asked of it. Never convicts: with no ground truth for the population, a
//!   conviction would rest on an inferred one, and inferring the population from
//!   a field name is exactly what manufactures the false verdicts this module
//!   exists to stop.
//!
//! Two structural asymmetries encode "silence is the weaker claim", matching
//! the reporting contract's own defaults:
//!
//! 1. **Synonymy is declared, never guessed.** A declaration that lists one
//!    population term is authoritative about *qualifiers of that term's head*
//!    but says nothing about synonyms of it, so an unlisted subject with a
//!    different head is [`PopulationAgreement::Undeclared`], not a conviction.
//!    A declaration that *enumerates* alternative surface forms (two or more,
//!    separated by [`POPULATION_TERM_SEPARATORS`]) is taken to be closed over
//!    surface forms, so an unlisted subject then disagrees. Strictness is
//!    therefore opt-in by completeness of the declaration, and an author can
//!    never be convicted for a synonym they were never asked to declare.
//! 2. **A derived subject is never taken for a population.** A count over a
//!    *product* of two populations (the entries of a two-dimensional container,
//!    a per-unit rate) is not the cardinality of either factor, whatever the
//!    synonymy, so a product-marked subject can never agree — even when the
//!    subject phrase textually contains the declared term.
//!
//! Known limits, stated so a caller does not over-trust a verdict. A clause
//! qualifier is only attested when it is *hyphen-attached* to the subject's
//! head (`evidence-row`): a space-separated modifier is ignored, because
//! participles and evaluative adjectives sit there far more often than
//! population qualifiers do, and honouring them would convict on wording
//! rather than on granularity. The consequence is one-directional — an
//! unhyphenated granularity statement degrades to
//! [`PopulationAgreement::Agrees`] or [`PopulationAgreement::Undeclared`],
//! never to a conviction. Conversely a deliberately hyphenated *participle*
//! (`filtered-row`) counts as a qualifier and can conflict with a declared one.
//! Qualifier attestation is clause-scoped, so a clause that names two
//! distinct hyphenated sub-populations of one head is ambiguous and returns
//! [`PopulationAgreement::Undeclared`] rather than picking one.

use std::collections::BTreeSet;
use std::sync::LazyLock;

/// Characters that separate a declaration into alternative surface forms of one
/// population, e.g. a `population` of `input_records | postings`.
///
/// This is the only channel available for population synonymy in the current
/// `Observable` shape, whose `aliases` field is defined over *key paths* and
/// therefore cannot carry it. Splitting on punctuation keeps the accepted terms
/// inside the declaration — the invariant this module must not break is that no
/// synonym is ever supplied by this source file.
pub const POPULATION_TERM_SEPARATORS: [char; 4] = ['|', '/', ',', ';'];

/// Whitespace words after the subject's head that are inspected for a
/// product marker, plus the one word before it. Kept to a tight window for the
/// same reason the dimensional filter keeps its modifier window tight: a marker
/// further away belongs to another quantity in the same sentence, and honouring
/// it at a distance would veto a sound binding.
const HEAD_WINDOW_FOLLOWING_WORDS: usize = 2;

/// Whitespace words either side of the subject's head that are inspected for a
/// spelled-out numeric derivation of the quoted count. Wider than the marker
/// window because the derivation is conventionally a trailing parenthetical, and
/// symmetric because it is occasionally a leading equation — but still local, so
/// an unrelated parenthetical elsewhere in the sentence cannot be mistaken for
/// the derivation of *this* count.
const DERIVATION_WINDOW_WORDS: usize = 3;

/// Structural markers that the counted subject is a *product* over two
/// populations rather than a cardinality of one: a hyphenated dimension
/// compound (`A-by-B`), an explicit multiplication operator between two
/// operands, or a per-unit rate.
///
/// Vocabulary-free by construction — an operator, the hyphenated connective,
/// and the per-unit preposition are grammar, not subject matter — so the
/// marker set carries over to any domain unchanged.
static COMPOSITE_SUBJECT_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?ix)
            -\s*by\s*-
          | \s (?: × | ⨯ | \* | x ) \s
          | \b per \b
        ",
    )
    .expect("static regex")
});

/// A numeric product presented as the *derivation* of the quoted count, i.e.
/// parenthesized or introduced by an equals sign (`(22369 x 8)`).
///
/// Two guards keep this from vetoing sound bindings, and both are load-bearing.
/// The parenthesis/equals anchor: an unanchored numeral pair around a
/// multiplication operator spells out configuration at least as often as a
/// derivation. And locality — it is only ever matched inside
/// [`DERIVATION_WINDOW_WORDS`] of the subject's head, so a parenthetical
/// belonging to some other quantity in the sentence is not mistaken for the
/// derivation of this count.
static DERIVED_PRODUCT_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?ix) [\(=] \s* \d [\d,\.]* \s* (?: × | ⨯ | \* | x ) \s* \d")
        .expect("static regex")
});

/// A hyphenated compound in prose, used to attest a qualifier of the subject's
/// head (`evidence-row`). Alphanumeric components only, so a dash used as
/// punctuation cannot create a spurious compound.
static HYPHENATED_COMPOUND_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[[:alnum:]]+(?:-[[:alnum:]]+)+").expect("static regex"));

/// Whether a narrative clause's counted subject agrees with a declared
/// observable's population.
///
/// The three verdicts are not a confidence scale: they are *who knows what*.
/// [`Self::Agrees`] and [`Self::Disagrees`] both require a declaration to have
/// answered the question asked of it; [`Self::Undeclared`] is the answer
/// whenever it did not, and it is the verdict every unrecognized input must
/// fall to so that an unfamiliar wording can only cost a finding, never
/// fabricate one.
///
/// `#[non_exhaustive]` because a further verdict (a declared *superset*
/// relation, say) must stay a minor change for downstream consumers.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopulationAgreement {
    /// The clause's counted subject is a surface form the declaration accepts,
    /// either as its population term or as a declared alternative. The absence
    /// of an objection, not support for the binding.
    Agrees,
    /// The declaration and the clause each identify a population and the two
    /// are different: a different head, or the same head under conflicting
    /// qualifiers, or a subject that is a product over populations rather than
    /// a population. The only verdict [`may_convict`] admits.
    Disagrees,
    /// No declaration, an unidentifiable subject, or a declaration that is
    /// silent about the subject it was asked about. Carries no information, and
    /// therefore can never support a finding.
    Undeclared,
}

/// Whether a narrative clause's counted subject agrees with the population
/// declared for the field the count resolved to.
///
/// `declared_population` is the `Observable::population` string for that field,
/// and `None` — or a string with no term in it — means the stage declared no
/// observable for the count, which is [`PopulationAgreement::Undeclared`].
///
/// Enforces three invariants, in the order they are checked:
///
/// 1. **No declaration, no verdict.** Without a declared population there is no
///    ground truth for the counted subject, so nothing beyond
///    [`PopulationAgreement::Undeclared`] can be returned. This is what stops a
///    granularity conviction from resting on a population inferred out of a
///    field name.
/// 2. **A product is not a population.** A subject marked as a product over two
///    populations — a hyphenated dimension compound, an explicit multiplication
///    operator, a per-unit rate, or a parenthesized numeric derivation of the
///    quoted count — never agrees with either factor, so the count of a
///    rectangular container's entries cannot pass for the cardinality of its
///    rows or of its columns. Checked *before* term matching, because such a
///    subject phrase typically contains the declared term and would otherwise
///    match it.
/// 3. **Synonymy is declared, not guessed.** A subject whose head differs from
///    every declared head disagrees only when the declaration *enumerated* its
///    accepted surface forms; a single-term declaration is silent about
///    synonyms and yields [`PopulationAgreement::Undeclared`]. A subject
///    sharing a declared head is decided by qualifier compatibility, which
///    needs no synonym channel: one qualifier set containing the other is the
///    same population stated at two levels of specificity, while two
///    conflicting sets are two sub-populations of one head.
///
/// Agreement is reported if *any* candidate interpretation of the subject
/// agrees, so the function biases toward silence: an ambiguous clause degrades
/// to [`PopulationAgreement::Undeclared`] instead of convicting on whichever
/// interpretation happens to conflict.
#[must_use]
pub fn agreement(
    clause: &str,
    noun: &str,
    declared_population: Option<&str>,
) -> PopulationAgreement {
    let declared = declared_terms(declared_population);
    if declared.is_empty() {
        return PopulationAgreement::Undeclared;
    }
    let Some(subject) = subject_of(clause, noun) else {
        return PopulationAgreement::Undeclared;
    };
    if subject.is_product {
        return PopulationAgreement::Disagrees;
    }
    let enumerated = declared.len() > 1;
    let mut saw_disagreement = false;
    for candidate in &subject.candidates {
        for term in &declared {
            match compare_phrases(candidate, term, enumerated) {
                PopulationAgreement::Agrees => return PopulationAgreement::Agrees,
                PopulationAgreement::Disagrees => saw_disagreement = true,
                PopulationAgreement::Undeclared => {}
            }
        }
    }
    if subject.ambiguous {
        return PopulationAgreement::Undeclared;
    }
    if saw_disagreement {
        PopulationAgreement::Disagrees
    } else {
        PopulationAgreement::Undeclared
    }
}

/// Whether the population channel alone permits this binding to produce a
/// Mismatch.
///
/// Only [`PopulationAgreement::Disagrees`] does. The other two verdicts carry
/// no population objection: [`PopulationAgreement::Agrees`] is the absence of
/// one (it is not itself evidence of anything, so it cannot support a finding
/// on population grounds), and [`PopulationAgreement::Undeclared`] is the
/// absence of a *declaration* — with no declared population there is no ground
/// truth for the counted subject, and convicting on an inferred one is what
/// manufactured the false convictions this module exists to prevent.
///
/// The invariant a caller must preserve: `false` means "this module raises no
/// population objection", never "this binding is verified" and never "no
/// finding of any other kind is permitted". A numeric disagreement on an
/// [`PopulationAgreement::Agrees`] binding remains entirely the caller's
/// business.
#[must_use]
pub fn may_convict(agreement: PopulationAgreement) -> bool {
    matches!(agreement, PopulationAgreement::Disagrees)
}

/// The candidate interpretations of a clause's counted subject, plus the two
/// facts about it that decide a verdict before any term is matched.
struct Subject {
    /// One or more normalized token phrases the subject may denote. More than
    /// one only when the clause attests several hyphenated qualifiers of the
    /// same head.
    candidates: Vec<Vec<String>>,
    /// Set when several distinct qualifiers are attested, so no single
    /// interpretation is warranted and a non-agreeing outcome must stay
    /// [`PopulationAgreement::Undeclared`].
    ambiguous: bool,
    /// Set when the subject is a product over two populations rather than a
    /// population, in which case it can agree with nothing.
    is_product: bool,
}

/// The accepted surface forms of a declared population, in declaration order,
/// de-duplicated so that "enumerated" counts *distinct* terms.
///
/// Returning an empty vector for a `None`, blank, or separator-only declaration
/// is what makes an absent declaration indistinguishable from an empty one:
/// both are silence, and silence never convicts.
fn declared_terms(declared_population: Option<&str>) -> Vec<Vec<String>> {
    let Some(text) = declared_population else {
        return Vec::new();
    };
    let mut seen: BTreeSet<Vec<String>> = BTreeSet::new();
    let mut terms: Vec<Vec<String>> = Vec::new();
    for part in text.split(&POPULATION_TERM_SEPARATORS[..]) {
        let tokens = population_tokens(part);
        if tokens.is_empty() {
            continue;
        }
        if seen.insert(tokens.clone()) {
            terms.push(tokens);
        }
    }
    terms
}

/// The candidate interpretations of the counted subject, or `None` when the
/// subject
/// carries no identifiable term at all (an empty noun, or one made only of
/// numerals) — the case that must fall to
/// [`PopulationAgreement::Undeclared`].
///
/// A noun the extractor already recorded as a compound is used as-is. A bare
/// noun is qualified from the clause, but only by a hyphen-attached component
/// of the same head, and the most specific attestation wins: once any qualifier
/// is attested, the bare interpretation is dropped, so a clause that states its
/// own granularity is not also allowed to match a declared qualifier vacuously.
fn subject_of(clause: &str, noun: &str) -> Option<Subject> {
    let raw = noun
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    let tokens = population_tokens(&raw);
    let head = tokens.last()?.clone();
    let is_product = COMPOSITE_SUBJECT_RE.is_match(&raw) || product_marked_near_head(clause, &head);
    if tokens.len() > 1 {
        return Some(Subject {
            candidates: vec![tokens],
            ambiguous: false,
            is_product,
        });
    }
    let attested = attested_qualifiers(clause, &head);
    if attested.is_empty() {
        return Some(Subject {
            candidates: vec![tokens],
            ambiguous: false,
            is_product,
        });
    }
    Some(Subject {
        ambiguous: attested.len() > 1,
        candidates: attested.into_iter().collect(),
        is_product,
    })
}

/// The distinct hyphen-attested qualified phrases for `head` in the clause,
/// each as `qualifier… ++ head`.
///
/// A compound whose qualifying components are all numerals ("23-row") attests
/// nothing: the numeral is the quantity, not a population qualifier. Ordered
/// (`BTreeSet`) so the candidate list — and therefore every verdict — is
/// identical on every run.
fn attested_qualifiers(clause: &str, head: &str) -> BTreeSet<Vec<String>> {
    let mut attested: BTreeSet<Vec<String>> = BTreeSet::new();
    for found in HYPHENATED_COMPOUND_RE.find_iter(clause) {
        let parts: Vec<String> = found
            .as_str()
            .split('-')
            .map(|part| singular(&part.to_lowercase()))
            .collect();
        let Some((last, leading)) = parts.split_last() else {
            continue;
        };
        if last != head {
            continue;
        }
        let mut phrase: Vec<String> = leading
            .iter()
            .filter(|part| !is_numeral(part))
            .cloned()
            .collect();
        if phrase.is_empty() {
            continue;
        }
        phrase.push(head.to_string());
        attested.insert(phrase);
    }
    attested
}

/// Whether a product marker, or a spelled-out numeric derivation, sits in the
/// clause window around an occurrence of the subject's head.
///
/// Two windows, because the two signals carry different false-positive risk. A
/// bare marker is only honoured one word before and
/// [`HEAD_WINDOW_FOLLOWING_WORDS`] after the head — enough for the qualifying
/// form ("A-by-B entries") and the trailing form ("entries per B") — because
/// the per-unit preposition is common prose and a distant one governs some
/// other quantity. An anchored numeric derivation is honoured within
/// [`DERIVATION_WINDOW_WORDS`] either side, since it is already
/// self-identifying and is conventionally set a few words off in parentheses.
fn product_marked_near_head(clause: &str, head: &str) -> bool {
    let words: Vec<&str> = clause.split_whitespace().collect();
    for (index, word) in words.iter().enumerate() {
        if !population_tokens(word).iter().any(|token| token == head) {
            continue;
        }
        if COMPOSITE_SUBJECT_RE.is_match(&window(&words, index, 1, HEAD_WINDOW_FOLLOWING_WORDS)) {
            return true;
        }
        if DERIVED_PRODUCT_RE.is_match(&window(
            &words,
            index,
            DERIVATION_WINDOW_WORDS,
            DERIVATION_WINDOW_WORDS,
        )) {
            return true;
        }
    }
    false
}

/// The whitespace-word window around `index`, `preceding` words before and
/// `following` words after, clamped to the clause. Rejoined with single spaces
/// so a marker that needs whitespace on both sides is still detectable at a
/// window edge.
fn window(words: &[&str], index: usize, preceding: usize, following: usize) -> String {
    let start = index.saturating_sub(preceding);
    let end = (index + 1 + following).min(words.len());
    words[start..end].join(" ")
}

/// Verdict for one candidate interpretation of the subject against one declared
/// term.
///
/// The head — the last token — carries the population's identity, and the
/// leading tokens qualify it. Same head with one qualifier set containing the
/// other is one population named at two levels of specificity; same head with
/// conflicting qualifier sets is two sub-populations, which is decidable from a
/// single declared term and needs no synonym channel. Different heads are
/// decidable only against an enumerated declaration: with one declared term
/// there is no way to tell a synonym of it from a different population, and
/// guessing is precisely the failure this module must not commit.
fn compare_phrases(subject: &[String], term: &[String], enumerated: bool) -> PopulationAgreement {
    let (Some(subject_head), Some(term_head)) = (subject.last(), term.last()) else {
        return PopulationAgreement::Undeclared;
    };
    if subject_head != term_head {
        return if enumerated {
            PopulationAgreement::Disagrees
        } else {
            PopulationAgreement::Undeclared
        };
    }
    let subject_qualifiers: BTreeSet<&String> = subject[..subject.len() - 1].iter().collect();
    let term_qualifiers: BTreeSet<&String> = term[..term.len() - 1].iter().collect();
    if subject_qualifiers.is_subset(&term_qualifiers)
        || term_qualifiers.is_subset(&subject_qualifiers)
    {
        PopulationAgreement::Agrees
    } else {
        PopulationAgreement::Disagrees
    }
}

/// The population-bearing tokens of a fragment: lowercased, split on every
/// non-alphanumeric character so snake_case, kebab-case and prose tokenize
/// identically, numerals dropped, each token singularized.
///
/// Dropping numerals is what makes "23-row" and "row" the same subject; number
/// folding is what makes a declared `input_records` accept a narrated
/// "input record" without either side listing both forms.
fn population_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .filter(|token| !is_numeral(token))
        .map(|token| singular(&token))
        .collect()
}

/// Whether a token is purely numeric, and so quantifies the subject instead of
/// naming it.
fn is_numeral(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_ascii_digit())
}

/// Regular-plural singularization, so a declared term and a narrated one are
/// compared as the same word regardless of number.
///
/// Latin-looking `-us`/`-is` endings and doubled `-ss` are left alone because
/// they are not plurals; every suffix inspected is ASCII, so the byte slicing
/// stays on character boundaries.
fn singular(token: &str) -> String {
    if token.len() > 4 && token.ends_with("ies") {
        return format!("{}y", &token[..token.len() - 3]);
    }
    if token.len() > 4 && token.ends_with("es") {
        let stem = &token[..token.len() - 2];
        if stem.ends_with(['s', 'x', 'z', 'h']) {
            return stem.to_string();
        }
    }
    if token.len() > 3
        && token.ends_with('s')
        && !token.ends_with("ss")
        && !token.ends_with("us")
        && !token.ends_with("is")
    {
        return token[..token.len() - 1].to_string();
    }
    token.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every verdict, for the relation-level properties. Exhaustive by hand
    /// because the enum is `#[non_exhaustive]`.
    const ALL_VERDICTS: [PopulationAgreement; 3] = [
        PopulationAgreement::Agrees,
        PopulationAgreement::Disagrees,
        PopulationAgreement::Undeclared,
    ];

    /// A well-authored declaration for the retained-row cardinality of a
    /// prefiltering stage: the population plus the surface form its own
    /// narrative uses for it. Both observed verdicts for that field follow from
    /// this one declaration.
    const RETAINED_POPULATION: Option<&str> = Some("features | genes");

    /// The declaration of the literature-concordance stage's row cardinality.
    /// One term, because the qualifier conflict it must catch is decidable
    /// without any enumeration.
    const FEATURE_ROW_POPULATION: Option<&str> = Some("feature_rows");

    /// Observed false conviction: a narrative reporting the cell count of a
    /// retained matrix (178952) was bound to the retained-row cardinality
    /// (22369). Both are genuine counts, so no dimensional filter separates
    /// them; the declared population does.
    #[test]
    fn matrix_cell_count_disagrees_with_the_declared_retained_population() {
        let clause = "reproduced every one of the 178,952 cells of the retained vst_matrix.tsv";
        assert_eq!(
            agreement(clause, "cells", RETAINED_POPULATION),
            PopulationAgreement::Disagrees,
            "a cell count is not the retained-feature cardinality"
        );
        assert!(
            may_convict(agreement(clause, "cells", RETAINED_POPULATION)),
            "a declared population disagreement is the one convictable verdict"
        );
    }

    /// Observed false conviction: a 23-row evidence table was bound to the
    /// 63677 feature rows of the same stage. Same head, conflicting
    /// qualifiers — decidable from the single declared term.
    #[test]
    fn evidence_row_granularity_disagrees_with_the_declared_feature_row_population() {
        let clause = "The 23-row literature-concordance table is at evidence-row granularity";
        assert_eq!(
            agreement(clause, "row", FEATURE_ROW_POPULATION),
            PopulationAgreement::Disagrees,
            "an evidence row is not a feature row"
        );
        assert_eq!(
            agreement(clause, "evidence-row", FEATURE_ROW_POPULATION),
            PopulationAgreement::Disagrees,
            "the same verdict when the extractor recorded the compound noun"
        );
    }

    /// The correct binding that must survive, and the reason naive noun
    /// agreement cannot be used: the narrative's noun differs from the field's
    /// population term, and only the declaration knows they are the same
    /// population.
    #[test]
    fn a_declared_surface_form_makes_the_correct_binding_agree() {
        let clause = "22369 genes were retained after prefiltering";
        assert_eq!(
            agreement(clause, "genes", RETAINED_POPULATION),
            PopulationAgreement::Agrees,
            "a declared surface form of the population is the population"
        );
        assert!(
            !may_convict(agreement(clause, "genes", RETAINED_POPULATION)),
            "agreement raises no population objection"
        );
    }

    /// The honest limit of the declaration as it stands. A single-term
    /// declaration is silent about synonyms, so the correct binding and the
    /// wrong one are indistinguishable and neither may convict. Strictness is
    /// bought by enumerating the surface forms, never by inference here.
    #[test]
    fn a_single_term_declaration_cannot_decide_a_different_head() {
        let bare = Some("features");
        assert_eq!(
            agreement(
                "reproduced every one of the 178,952 cells of the retained vst_matrix.tsv",
                "cells",
                bare
            ),
            PopulationAgreement::Undeclared,
            "without enumerated surface forms the wrong subject is undecidable"
        );
        assert_eq!(
            agreement(
                "22369 genes were retained after prefiltering",
                "genes",
                bare
            ),
            PopulationAgreement::Undeclared,
            "and so is the right one — the trap a wordlist would spring"
        );
    }

    /// A declaration is authoritative about qualifiers of its own head even
    /// with one term, and the containment direction does not matter: a bare
    /// subject is the same population stated less specifically.
    #[test]
    fn qualifier_containment_agrees_in_both_directions() {
        assert_eq!(
            agreement(
                "we ingested 1000 records in total",
                "records",
                Some("input_records")
            ),
            PopulationAgreement::Agrees,
            "a bare subject is the declared population, less specified"
        );
        assert_eq!(
            agreement(
                "the 23 input-record rejections were logged",
                "input-record",
                Some("records")
            ),
            PopulationAgreement::Agrees,
            "a qualified subject under an unqualified declaration still agrees"
        );
    }

    /// A count over a product of two populations is not the cardinality of
    /// either factor. Checked before term matching, because the product phrase
    /// usually contains the declared term and would otherwise match it.
    #[test]
    fn a_product_subject_is_never_read_as_either_factor() {
        assert_eq!(
            agreement(
                "all 178952 gene-by-sample values match the source",
                "values",
                Some("genes")
            ),
            PopulationAgreement::Disagrees,
            "a hyphenated dimension compound marks a product, not a population"
        );
        assert_eq!(
            agreement(
                "all 178952 feature-by-sample samples match",
                "feature-by-sample samples",
                Some("samples")
            ),
            PopulationAgreement::Disagrees,
            "a product phrase must not agree by containing the declared term"
        );
        assert_eq!(
            agreement(
                "every one of the 178952 cells (22369 x 8) matches",
                "cells",
                Some("features")
            ),
            PopulationAgreement::Disagrees,
            "a parenthesized numeric derivation states the product explicitly"
        );
        assert_eq!(
            agreement(
                "we observed 4200 counts per sample",
                "counts",
                Some("counts")
            ),
            PopulationAgreement::Disagrees,
            "a per-unit rate is derived, not a population cardinality"
        );
    }

    /// An unanchored numeral pair around a multiplication operator states a
    /// configuration rather than the derivation of the quoted count, so it must
    /// not be read as a product. The parenthesis/equals anchor is what
    /// separates the two, and a marker outside the head's window is ignored.
    #[test]
    fn an_unanchored_numeral_pair_is_not_a_derivation() {
        assert_eq!(
            agreement(
                "we sequenced 2 x 100 base pair fragments and retained 412 reads",
                "reads",
                Some("reads")
            ),
            PopulationAgreement::Agrees,
            "an unanchored operator pair away from the head is configuration"
        );
        assert_eq!(
            agreement(
                "we retained 412 reads after trimming (2 x 100 cycles)",
                "reads",
                Some("reads")
            ),
            PopulationAgreement::Agrees,
            "an anchored pair beyond the subject's window is another quantity's"
        );
        assert_eq!(
            agreement(
                "we retained 412 reads (206 x 2) in total",
                "reads",
                Some("reads")
            ),
            PopulationAgreement::Disagrees,
            "the same pair anchored beside the subject is its derivation"
        );
    }

    /// Every path with nothing to decide on returns the non-convicting verdict:
    /// no declaration, an empty or separator-only declaration, an empty noun, a
    /// numeral-only noun, and a punctuation-only noun.
    #[test]
    fn every_unknown_path_is_undeclared_and_never_convicts() {
        let clause = "22369 genes were retained after prefiltering";
        for verdict in [
            agreement(clause, "genes", None),
            agreement(clause, "genes", Some("")),
            agreement(clause, "genes", Some("   ")),
            agreement(clause, "genes", Some("|,;/")),
            agreement(clause, "genes", Some("42")),
            agreement(clause, "", RETAINED_POPULATION),
            agreement(clause, "22369", RETAINED_POPULATION),
            agreement(clause, "(...)", RETAINED_POPULATION),
        ] {
            assert_eq!(
                verdict,
                PopulationAgreement::Undeclared,
                "an undecidable input must not produce a verdict"
            );
            assert!(
                !may_convict(verdict),
                "an undecidable input must never convict"
            );
        }
    }

    /// Two distinct hyphenated qualifiers of one head are ambiguous: the clause
    /// warrants no single reading, so the non-agreeing outcome stays silent
    /// instead of convicting on whichever reading conflicts.
    #[test]
    fn conflicting_attestations_are_ambiguous_not_convictable() {
        let clause = "the evidence-row table and the summary-row table were both written";
        assert_eq!(
            agreement(clause, "row", FEATURE_ROW_POPULATION),
            PopulationAgreement::Undeclared,
            "two attested qualifiers warrant no single subject reading"
        );
        let with_declared = "the evidence-row table restates the feature-row totals";
        assert_eq!(
            agreement(with_declared, "row", FEATURE_ROW_POPULATION),
            PopulationAgreement::Agrees,
            "an attested reading that matches the declaration wins over a conflicting one"
        );
    }

    /// A numeral-qualified compound attests nothing — the numeral is the
    /// quantity, not a qualifier — so the subject stays the bare head.
    #[test]
    fn a_numeral_qualified_compound_attests_no_qualifier() {
        assert_eq!(
            agreement("the 23-row table was written", "row", Some("rows")),
            PopulationAgreement::Agrees,
            "'23-row' is the declared population, quantified"
        );
    }

    /// Number folding means neither side has to list both forms of a word, and
    /// a declaration written in snake_case compares as its words.
    #[test]
    fn plural_and_snake_case_forms_compare_as_one_term() {
        assert_eq!(
            agreement("one entry was rejected", "entry", Some("entries")),
            PopulationAgreement::Agrees,
            "an irregular-looking regular plural folds to its singular"
        );
        assert_eq!(
            agreement(
                "63677 feature rows were emitted",
                "feature rows",
                FEATURE_ROW_POPULATION
            ),
            PopulationAgreement::Agrees,
            "a snake_case declaration compares as its words"
        );
    }

    /// The generalization proof: nothing in this module knows a field of study,
    /// so a declaration authored for freight works exactly as one authored for
    /// any other domain — including the enumerated-surface-form, qualifier-
    /// conflict, product and single-term-silence paths.
    #[test]
    fn generalizes_to_a_non_biological_domain() {
        let enumerated = Some("shipments | consignments");
        assert_eq!(
            agreement(
                "all 412 consignments cleared customs",
                "consignments",
                enumerated
            ),
            PopulationAgreement::Agrees,
            "an enumerated surface form agrees"
        );
        assert_eq!(
            agreement("all 4944 pallets were scanned", "pallets", enumerated),
            PopulationAgreement::Disagrees,
            "an unlisted subject disagrees with an enumerated declaration"
        );
        assert_eq!(
            agreement(
                "the 4944 pallet-by-shipment placements were scanned",
                "placements",
                enumerated
            ),
            PopulationAgreement::Disagrees,
            "a product over two populations is neither of them"
        );
        assert_eq!(
            agreement(
                "the 23-row exception log is at rejected-row granularity",
                "row",
                Some("accepted_rows")
            ),
            PopulationAgreement::Disagrees,
            "a qualifier conflict needs no enumeration"
        );
        assert_eq!(
            agreement(
                "all 412 consignments cleared customs",
                "consignments",
                Some("shipments")
            ),
            PopulationAgreement::Undeclared,
            "a single-term declaration stays silent about synonyms"
        );
    }

    /// The gate itself: exactly one verdict may support a finding, and the two
    /// that carry no information may not.
    #[test]
    fn only_a_declared_disagreement_may_convict() {
        for verdict in ALL_VERDICTS {
            assert_eq!(
                may_convict(verdict),
                verdict == PopulationAgreement::Disagrees,
                "only a declared disagreement may convict, got {verdict:?}"
            );
        }
    }

    /// Verdicts must not depend on run-to-run iteration order, since a
    /// conviction that appears intermittently is worse than none.
    #[test]
    fn verdicts_are_deterministic_across_repeated_calls() {
        let clause = "The 23-row literature-concordance table is at evidence-row granularity";
        let first = agreement(clause, "row", FEATURE_ROW_POPULATION);
        for _ in 0..16 {
            assert_eq!(
                agreement(clause, "row", FEATURE_ROW_POPULATION),
                first,
                "repeated adjudication of one binding must agree with itself"
            );
        }
    }
}
