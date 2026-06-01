"""Inter-judge agreement on ordinal A/B/C verdicts (exact + linear-weighted kappa)."""
from __future__ import annotations
_ORD = {"A": 0, "B": 1, "C": 2}

def per_criterion_exact(a: dict, b: dict) -> float:
    keys = set(a) & set(b)
    if not keys:
        return 0.0
    return sum(1 for k in keys if a[k] == b[k]) / len(keys)

def agreement_overlap(a: dict, b: dict) -> dict:
    """Describe how much the two judges' criterion sets overlap.

    ``per_criterion_exact``/``linear_weighted_kappa`` score only the key
    intersection, so a partial verdict (one judge returning fewer criterion
    ids) can read as high agreement on a thin overlap. Callers consult this to
    tell whether the agreement figure spans the whole rubric (``complete``) or
    just a subset.
    """
    ka, kb = set(a), set(b)
    return {
        "n_overlap": len(ka & kb),
        "n_union": len(ka | kb),
        "complete": ka == kb,
    }

def linear_weighted_kappa(a: dict, b: dict) -> float:
    keys = sorted(set(a) & set(b))
    if not keys:
        return 0.0
    xs = [_ORD[a[k]] for k in keys]; ys = [_ORD[b[k]] for k in keys]
    n = len(keys); cats = [0, 1, 2]; k = len(cats)
    w = [[abs(i - j) / (k - 1) for j in cats] for i in cats]
    import collections
    O = [[0.0] * k for _ in cats]
    for x, y in zip(xs, ys):
        O[x][y] += 1
    rx = collections.Counter(xs); ry = collections.Counter(ys)
    num = sum(w[i][j] * O[i][j] for i in cats for j in cats)
    den = sum(w[i][j] * (rx[i] * ry[j] / n) for i in cats for j in cats)
    if den == 0:
        return 1.0
    return 1.0 - num / den
