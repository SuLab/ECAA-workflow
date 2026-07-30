"""Direction inference over retrieved literature text.

Given a sentence that names an entity, decide whether the sentence reports
that entity going UP or DOWN. The lexicon below is the whole of the
decision procedure — there is no model, no memory, and no per-run literal.

What a derived direction does and does not mean
-----------------------------------------------
`infer_direction` answers "does this sentence report an increase or a
decrease involving this entity". It does NOT answer "under the same
contrast as the analysis". A paper reporting that a gene is induced by one
stimulus says nothing about a different stimulus, and this module cannot
tell the two apart from prose alone.

`contrast_terms` is the caller's lever on that gap: supply the vocabulary
of the analysis contrast (the perturbation, the tissue, the comparison)
and a sentence must mention at least one of those terms before its
direction is accepted. Without them the direction is still derived, but
`contrast_grounded` comes back `False` and the caller is expected to say
so in its verification report rather than quietly presenting an ungrounded
direction as a replication check.

`entity` is the caller's lever on cue attribution. When supplied, a cue
must occur near the named entity. This deliberately turns sentences such
as "IL6R was influenced by CEBPD knockdown" into unresolved evidence
rather than assigning the perturbation's direction to IL6R.

Ambiguity resolves to "no direction", never to a guess: a sentence with
cues in both classes, or with none, yields `None` — which the caller maps
to the `unverifiable` concordance flag.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import List, Optional, Sequence, Tuple

#: Stems reporting an INCREASE. Matched as prefixes on word boundaries, so
#: `induc` covers induce/induced/induces/induction/inducible.
#:
#: `stimulat` and `activat` are deliberately absent. In experimental prose
#: they name the PERTURBATION far more often than the measured outcome
#: ("after LPS stimulation", "T-cell activation"), so including them both
#: manufactures spurious "up" calls and, where a genuine down-cue is also
#: present, collapses resolvable sentences into ambiguity.
UP_STEMS: Tuple[str, ...] = (
    "induc",
    "upregulat",
    "up-regulat",
    "increas",
    "elevat",
    "enhanc",
    "augment",
    "higher",
    "greater",
    "overexpress",
    "accumulat",
)

#: Stems reporting a DECREASE.
DOWN_STEMS: Tuple[str, ...] = (
    "downregulat",
    "down-regulat",
    "repress",
    "decreas",
    "reduc",
    "suppress",
    "inhibit",
    "attenuat",
    "diminish",
    "lower",
    "abolish",
    "depleted",
)

# `knockdown`, `knockout`, and `loss of` are deliberately absent. Like
# stimulation and activation, they usually name an experimental perturbation,
# not the direction of the measured entity. A sentence must contain an actual
# outcome cue such as reduced, repressed, or lower to support a down call.

#: Negators that cancel a cue appearing AFTER them, within
#: `NEGATION_WINDOW` characters: "did not significantly increase".
PRECEDING_NEGATORS: Tuple[str, ...] = (
    "not",
    "no",
    "never",
    "neither",
    "nor",
    "without",
    "failed to",
    "fails to",
    "unable to",
    "did not",
    "does not",
    "was not",
    "were not",
)

#: Terms that void the WHOLE sentence's direction wherever they appear.
#:
#: A windowed look-behind is not enough for these. "Dexamethasone-induced
#: DUSP1 mRNA was unaffected." puts the cue (`induc`) at the very start and
#: the negation at the very end — scanning backwards from the cue finds
#: nothing, and the sentence scores as an increase when it reports the exact
#: opposite. These terms describe the OUTCOME, so their scope is the
#: sentence, not a window.
VOIDING_TERMS: Tuple[str, ...] = (
    "unaffected",
    "unchanged",
    "no change",
    "no changes",
    "no difference",
    "no differences",
    "no significant change",
    "no significant difference",
    "no significant effect",
    "no effect",
    "without effect",
    "not altered",
    "not significantly",
    "nonsignificant",
    "non-significant",
    "not significant",
)

#: How many characters before a cue are scanned for a preceding negator.
#: Wide enough for "did not significantly increase", narrow enough that a
#: negation in a neighbouring clause does not reach across.
NEGATION_WINDOW = 40

# Maximum gap between a directional cue and an explicitly supplied entity.
# The bounded window is conservative by design: a distant cue may belong to
# another subject in the same sentence, so unresolved is safer than a false
# concordance verdict.
ENTITY_CUE_WINDOW = 48

UP = "up"
DOWN = "down"

#: The closed set of concordance flags, mirroring the atom's declared
#: `concordance_flag` enum. Exported so callers validate against one list.
CONCORDANCE_FLAGS: Tuple[str, ...] = (
    "same_direction",
    "opposite_direction",
    "no_prior_finding",
    "unverifiable",
    "not_assessed",
)


@dataclass(frozen=True)
class DirectionCall:
    """The outcome of scanning one sentence for one entity."""

    direction: Optional[str]
    """`"up"`, `"down"`, or `None` when the sentence is ambiguous or silent."""

    contrast_grounded: bool
    """`True` iff the sentence mentioned at least one caller-supplied
    contrast term. Always `False` when no contrast terms were supplied."""

    cue: Optional[str]
    """The stem that produced the call, for the verification report."""


def _phrase(term: str) -> re.Pattern:
    """Word-boundary matcher for a negation phrase.

    Substring matching is not safe here: the negator `no` occurs inside
    `known`, `cannot`, and `phenomenon`, and would silently cancel a cue in
    any sentence containing one of them.
    """
    return re.compile(rf"(?<![a-z]){re.escape(term)}(?![a-z])")


_PRECEDING_NEGATOR_RE = [_phrase(t) for t in PRECEDING_NEGATORS]
_VOIDING_RE = [_phrase(t) for t in VOIDING_TERMS]


def is_voided(text_lower: str) -> bool:
    """`True` when a sentence reports no change, whatever cues it contains."""
    return any(rx.search(text_lower) for rx in _VOIDING_RE)


def _stem_hits(text_lower: str, stems: Sequence[str]) -> List[Tuple[int, str]]:
    """`(position, stem)` for each stem occurring un-negated in `text_lower`."""
    hits: List[Tuple[int, str]] = []
    for stem in stems:
        for match in re.finditer(rf"(?<![a-z]){re.escape(stem)}", text_lower):
            # "enhancer" names a regulatory element; it is not a conjugation
            # of "enhance" and must never become an increase cue.
            if stem == "enhanc" and text_lower[match.start() :].startswith(
                ("enhancer", "enhancers")
            ):
                continue
            start = match.start()
            window = text_lower[max(0, start - NEGATION_WINDOW) : start]
            if any(rx.search(window) for rx in _PRECEDING_NEGATOR_RE):
                continue
            hits.append((start, stem))
    return sorted(hits)


def infer_direction(
    sentence: str,
    *,
    contrast_terms: Sequence[str] = (),
    entity: Optional[str] = None,
) -> DirectionCall:
    """Classify one sentence as reporting an increase, a decrease, or neither.

    Cues in both classes cancel: a sentence saying one thing rose while
    another fell cannot be attributed to a single entity from word
    proximity alone, and guessing which cue "belongs" to the entity is
    exactly the kind of inference this library refuses to make.
    """
    lower = sentence.lower()
    grounded = bool(contrast_terms) and any(
        term.strip().lower() in lower for term in contrast_terms if term.strip()
    )
    if is_voided(lower):
        return DirectionCall(None, grounded, None)

    up_hits = _stem_hits(lower, UP_STEMS)
    down_hits = _stem_hits(lower, DOWN_STEMS)

    if entity and entity.strip():
        escaped = re.escape(entity.strip().lower())
        entity_spans = [
            match.span()
            for match in re.finditer(
                rf"(?<![a-z0-9]){escaped}(?![a-z0-9])",
                lower,
            )
        ]

        def near_entity(hit: Tuple[int, str]) -> bool:
            cue_start, stem = hit
            cue_end = cue_start + len(stem)
            return any(
                max(entity_start - cue_end, cue_start - entity_end, 0)
                <= ENTITY_CUE_WINDOW
                for entity_start, entity_end in entity_spans
            )

        up_hits = [hit for hit in up_hits if near_entity(hit)]
        down_hits = [hit for hit in down_hits if near_entity(hit)]

    if up_hits and not down_hits:
        return DirectionCall(UP, grounded, up_hits[0][1])
    if down_hits and not up_hits:
        return DirectionCall(DOWN, grounded, down_hits[0][1])
    return DirectionCall(None, grounded, None)


def effect_direction(effect: Optional[float]) -> Optional[str]:
    """Direction of an analysis effect: sign only. Zero and missing both
    yield `None` — a zero effect is not a direction."""
    if effect is None:
        return None
    try:
        value = float(effect)
    except (TypeError, ValueError):
        return None
    if value != value or value in (float("inf"), float("-inf")):  # NaN / inf
        return None
    if value > 0:
        return UP
    if value < 0:
        return DOWN
    return None


def concordance(
    analysis_direction: Optional[str],
    prior_direction: Optional[str],
) -> str:
    """Map a (analysis, prior) direction pair to a closed-set flag.

    Only the two-known case can produce a concordance verdict. A missing
    direction on either side is `unverifiable`: the pair was assessed and
    could not be resolved, which is distinct from `no_prior_finding` (the
    entity was searched and nothing came back) and from `not_assessed` (no
    query was ever issued). Those two are the caller's to assign — they
    describe retrieval, not text.
    """
    if analysis_direction is None or prior_direction is None:
        return "unverifiable"
    return "same_direction" if analysis_direction == prior_direction else "opposite_direction"
