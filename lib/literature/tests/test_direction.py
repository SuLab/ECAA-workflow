"""Direction-inference tests.

The negation cases are not hypothetical: an end-to-end run over the
deposited himes evidence produced `same_direction` for the sentence
"Dexamethasone-induced DUSP1 mRNA was unaffected." — the induction cue
sits at the start, the negation at the end, and a look-behind window sees
nothing. That is the exact false-positive class the voiding-term rule
exists to close.
"""

from __future__ import annotations

import pytest

from lib.literature.direction import (
    CONCORDANCE_FLAGS,
    DOWN,
    UP,
    concordance,
    effect_direction,
    infer_direction,
    is_voided,
)


# --- cue detection --------------------------------------------------------


@pytest.mark.parametrize(
    "sentence",
    [
        "Dexamethasone induced DUSP1 mRNA in airway smooth muscle.",
        "DUSP1 was upregulated after treatment.",
        "We observed increased DUSP1 transcript abundance.",
        "DUSP1 levels were elevated.",
        "Expression was higher in treated cells.",
    ],
)
def test_up_cues(sentence: str) -> None:
    assert infer_direction(sentence).direction == UP


@pytest.mark.parametrize(
    "sentence",
    [
        "DUSP1 was strongly repressed.",
        "Transcript abundance decreased markedly.",
        "The gene was downregulated in the treated arm.",
        "Levels were lower in treated samples.",
        "Signalling was attenuated.",
    ],
)
def test_down_cues(sentence: str) -> None:
    assert infer_direction(sentence).direction == DOWN


def test_stems_match_word_initially_only() -> None:
    """`(?<![a-z])` keeps `induc` from firing inside an unrelated word."""
    assert infer_direction("Reinduction was not the subject of study.").direction is None


# --- negation -------------------------------------------------------------


def test_trailing_voiding_term_cancels_a_leading_cue() -> None:
    call = infer_direction("Dexamethasone-induced DUSP1 mRNA was unaffected.")
    assert call.direction is None
    assert call.cue is None


@pytest.mark.parametrize(
    "sentence",
    [
        "Expression showed no significant change despite induction.",
        "Abundance was unchanged after treatment.",
        "There was no difference in induced expression.",
        "Treatment had no effect on the increase seen in controls.",
    ],
)
def test_voiding_terms_scope_the_whole_sentence(sentence: str) -> None:
    assert is_voided(sentence.lower())
    assert infer_direction(sentence).direction is None


def test_preceding_negator_cancels_a_following_cue() -> None:
    assert infer_direction("Treatment did not increase DUSP1 expression.").direction is None
    assert infer_direction("The drug failed to induce DUSP1.").direction is None


def test_negators_match_on_word_boundaries_not_substrings() -> None:
    """`no` occurs inside `known`, `cannot`, and `phenomenon`. Substring
    matching would silently cancel the cue in every such sentence."""
    assert infer_direction("It is well known that DUSP1 increases.").direction == UP
    assert infer_direction("This phenomenon reflects elevated DUSP1.").direction == UP


def test_negator_beyond_the_window_does_not_reach_the_cue() -> None:
    far = "Nothing was observed in the first cohort. " + "x" * 60 + " DUSP1 increased."
    assert infer_direction(far).direction == UP


# --- ambiguity ------------------------------------------------------------


def test_cues_in_both_classes_cancel_rather_than_guess() -> None:
    call = infer_direction(
        "The corticosteroid-inducible gene MKP-1 (DUSP1) mediates the repressive "
        "effects of steroids."
    )
    assert call.direction is None


def test_silent_sentence_yields_no_direction() -> None:
    assert infer_direction("DUSP1 is a dual-specificity phosphatase.").direction is None


def test_perturbation_vocabulary_is_not_an_up_cue() -> None:
    """`stimulat`/`activat` name the perturbation at least as often as the
    outcome, so they are deliberately outside the lexicon."""
    assert infer_direction("DUSP1 was measured after LPS stimulation.").direction is None
    assert infer_direction("T-cell activation was assessed.").direction is None


def test_perturbation_vocabulary_is_not_a_down_outcome() -> None:
    assert (
        infer_direction(
            "IL6R was influenced by CEBPD knockdown.",
            entity="IL6R",
        ).direction
        is None
    )
    assert infer_direction("DUSP1 loss of function was studied.").direction is None


def test_enhancer_noun_is_not_an_enhance_cue() -> None:
    sentence = "CCAAT/Enhancer Binding Protein D (CEBPD) modulates inflammatory responses."
    assert infer_direction(sentence, entity="CEBPD").direction is None


def test_entity_window_does_not_assign_a_distant_cue_to_the_gene() -> None:
    sentence = (
        "Genes with inducible GR occupancy and putative antiinflammatory "
        "properties included IRS2, APPL2, RAMP1, and MFGE8."
    )
    assert infer_direction(sentence, entity="MFGE8").direction is None
    assert infer_direction(sentence, entity="IRS2").direction is None


def test_entity_window_retains_a_local_directional_cue() -> None:
    sentence = "Dexamethasone treatment significantly increased CRISPLD2 mRNA."
    assert infer_direction(sentence, entity="CRISPLD2").direction == UP


@pytest.mark.parametrize(
    "sentence",
    [
        "Feature-A treatment increased endpoint-B abundance.",
        "Feature-A deficiency was associated with reduced endpoint-B.",
        "Knockdown of Feature-A reduced endpoint-B.",
    ],
)
def test_entity_intervention_role_does_not_inherit_downstream_direction(
    sentence: str,
) -> None:
    assert infer_direction(sentence, entity="Feature-A").direction is None


def test_entity_measurement_still_retains_its_own_direction() -> None:
    assert (
        infer_direction(
            "Treatment increased Feature-A abundance.",
            entity="Feature-A",
        ).direction
        == UP
    )
    assert (
        infer_direction(
            "Feature-A abundance was reduced after treatment.",
            entity="Feature-A",
        ).direction
        == DOWN
    )


@pytest.mark.parametrize(
    "sentence",
    [
        "Feature-A is a transcriptional repressor of endpoint B.",
        "Feature-A is an inhibitor of endpoint B activation.",
        (
            "We integrated data from Feature-A overexpression to identify endpoint B "
            "as both a Feature-A-regulated feature and a novel repressor of hypertrophy."
        ),
    ],
)
def test_entity_does_not_inherit_direction_from_agentive_function_noun(
    sentence: str,
) -> None:
    assert infer_direction(sentence, entity="Feature-A").direction is None


def test_passive_change_is_not_confused_with_agentive_function_noun() -> None:
    result = infer_direction("Feature-A was repressed after treatment.", entity="Feature-A")
    assert result.direction == DOWN


def test_entity_does_not_inherit_cue_across_contrasting_clause() -> None:
    call = infer_direction(
        "Feature-B was reduced while Feature-A was measured as a covariate.",
        entity="Feature-A",
    )
    assert call.direction is None


# --- contrast grounding ---------------------------------------------------


def test_contrast_grounding_requires_a_supplied_term() -> None:
    sentence = "Dexamethasone induced DUSP1 mRNA."
    assert infer_direction(sentence).contrast_grounded is False
    assert infer_direction(sentence, contrast_terms=["dexamethasone"]).contrast_grounded is True
    assert infer_direction(sentence, contrast_terms=["rapamycin"]).contrast_grounded is False


def test_contrast_grounding_does_not_change_the_direction() -> None:
    sentence = "Dexamethasone induced DUSP1 mRNA."
    assert infer_direction(sentence, contrast_terms=["rapamycin"]).direction == UP


# --- effect sign ----------------------------------------------------------


def test_effect_direction_is_sign_only() -> None:
    assert effect_direction(2.9) == UP
    assert effect_direction(-0.001) == DOWN


def test_zero_and_missing_effects_are_not_directions() -> None:
    assert effect_direction(0.0) is None
    assert effect_direction(None) is None
    assert effect_direction(float("nan")) is None
    assert effect_direction(float("inf")) is None
    assert effect_direction("not a number") is None


# --- concordance ----------------------------------------------------------


def test_concordance_pairs() -> None:
    assert concordance(UP, UP) == "same_direction"
    assert concordance(DOWN, DOWN) == "same_direction"
    assert concordance(UP, DOWN) == "opposite_direction"
    assert concordance(DOWN, UP) == "opposite_direction"


def test_missing_direction_is_unverifiable_not_novel() -> None:
    """`unverifiable` (a prior exists but says nothing directional) is a
    different claim from `no_prior_finding` (searched, nothing retrieved)
    and from `not_assessed` (never searched)."""
    assert concordance(UP, None) == "unverifiable"
    assert concordance(None, UP) == "unverifiable"
    assert concordance(None, None) == "unverifiable"


def test_concordance_output_is_inside_the_closed_set() -> None:
    for analysis in (UP, DOWN, None):
        for prior in (UP, DOWN, None):
            assert concordance(analysis, prior) in CONCORDANCE_FLAGS
