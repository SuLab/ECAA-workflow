"""Grounds the REMOVAL of the `median <= 0.5` AF-spectrum rule.

The Nekrutenko `variant_af_spectrum_plausible` validator (crates/harness) and the
`variant_calling.af_spectrum_median_bounded` contract assertion (config/
downstream-policy/validation-contract-variants.json) once required the called AF
spectrum's median to sit at the low end ("right-skewed for heteroplasmy"). That
rule was REMOVED because it contradicts the benchmark's own ground truth: mtDNA
called against the rCRS reference is HOMOPLASMY-DOMINATED. This test computes the
pooled AF median directly from the pinned ground-truth answer-key VCFs and proves
median > 0.5 — i.e. the old rule would have FAILED the truth set — so the removal
is grounded in data, not a comment. Skips when the pinned dataset is not present
in the local cache (no network fetch in CI).
"""
import glob
import gzip
import os
import statistics
from pathlib import Path

import pytest


def _gt_results_dir():
    """Resolve the pinned nekrutenko ground_truth/results dir from the local
    eval cache WITHOUT triggering a download. Mirrors how plugins/nekrutenko.py
    derives answer_key = handle / 'ground_truth/results'."""
    roots = []
    env = os.environ.get("ECAA_EVAL_CACHE_DIR")
    if env:
        roots.append(Path(env))
    roots += [
        Path.home() / ".ecaa-workflow" / "eval-cache",
        Path("/home/a/mounts/wadmin/home/a/benchmark_data/hf"),
        Path("/home/a/mounts/wadmin/home/a/benchmark_data"),
    ]
    for r in roots:
        if not r.exists():
            continue
        for hit in glob.glob(
            str(r / "**" / "nekrut*LLM-eval-paper*" / "ground_truth" / "results"),
            recursive=True,
        ):
            p = Path(hit)
            if list(p.glob("*.vcf.gz")):
                return p
    return None


def _pooled_af(results_dir: Path) -> list[float]:
    afs: list[float] = []
    for f in sorted(results_dir.glob("*.vcf.gz")):
        with gzip.open(f, "rt") as fh:
            for ln in fh:
                if ln.startswith("#"):
                    continue
                cols = ln.split("\t")
                if len(cols) < 8:
                    continue
                for kv in cols[7].split(";"):
                    if kv.startswith("AF="):
                        try:
                            afs.append(float(kv[3:].split(",")[0]))
                        except ValueError:
                            pass
    return afs


def test_gt_af_spectrum_is_homoplasmy_dominated():
    d = _gt_results_dir()
    if d is None:
        pytest.skip("nekrutenko ground-truth not in local cache (pinned dataset absent)")
    afs = _pooled_af(d)
    assert afs, "no AF values parsed from ground-truth VCFs"
    med = statistics.median(afs)
    frac_high = sum(1 for a in afs if a > 0.5) / len(afs)
    # If this ever fails, the dataset shape changed — revisit whether a median
    # bound is appropriate. As pinned (datasets.lock), the truth set is
    # homoplasmy-dominated, so a `median <= 0.5` rule rejects correct call sets.
    assert med > 0.5, f"GT pooled AF median {med:.4f} unexpectedly <= 0.5"
    assert frac_high > 0.5, f"GT >0.5 fraction {frac_high:.3f} unexpectedly a minority"
