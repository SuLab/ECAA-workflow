from scripts.eval.scoring.agreement import per_criterion_exact, linear_weighted_kappa

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
