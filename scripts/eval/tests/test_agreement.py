from scripts.eval.scoring.agreement import (
    per_criterion_exact,
    linear_weighted_kappa,
    agreement_overlap,
)

def test_exact_agreement():
    a = {"c1":"A","c2":"B","c3":"C"}; b = {"c1":"A","c2":"C","c3":"C"}
    assert per_criterion_exact(a, b) == 2/3

def test_kappa_perfect_and_disagreement():
    a = {"c1":"A","c2":"B","c3":"C"}
    assert linear_weighted_kappa(a, a) == 1.0
    b = {"c1":"B","c2":"C","c3":"C"}
    assert linear_weighted_kappa(a, b) < 1.0

def test_empty_overlap():
    assert per_criterion_exact({}, {}) == 0.0
    assert linear_weighted_kappa({}, {}) == 0.0

def test_agreement_overlap_complete():
    a = {"c1":"A","c2":"B","c3":"C"}; b = {"c1":"A","c2":"C","c3":"C"}
    ov = agreement_overlap(a, b)
    assert ov == {"n_overlap": 3, "n_union": 3, "complete": True}

def test_agreement_overlap_partial():
    a = {"c1":"A","c2":"B","c3":"C"}; b = {"c1":"A","c2":"C"}
    ov = agreement_overlap(a, b)
    assert ov["n_overlap"] == 2
    assert ov["n_union"] == 3
    assert ov["complete"] is False

def test_agreement_overlap_empty():
    ov = agreement_overlap({}, {})
    assert ov == {"n_overlap": 0, "n_union": 0, "complete": True}
