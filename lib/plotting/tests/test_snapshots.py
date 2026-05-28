"""Snapshot tests for the highest-stakes figure types.

Each snapshot fixture renders a known-input figure through the library
and compares the resulting bytes against a hash committed in
`tests/snapshot-hashes.json`. CI fails on any drift; intentional
visual changes update the hash file in the same PR.

This is the byte-level guard. A separate quality-scoring CI job
(`lib/plotting/quality_scorer.py`) covers font-type / DPI / file-size
WCAG checks against the same fixtures.

To re-baseline after an intentional change, run:

    pytest lib/plotting/tests/test_snapshots.py --update-hashes

The `--update-hashes` plugin writes the current hashes to
`snapshot-hashes.json` in place. Review the diff before committing.
"""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from typing import Callable, Dict

import numpy as np
import pytest


ROOT = Path(__file__).resolve().parents[3]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from lib.plotting.core import (  # noqa: E402
    anomaly_timeline,
    bar,
    diversity_violin,
    forecast_ribbon,
    forest,
    heatmap,
    kaplan_meier,
    manhattan,
    morans_i_scatter,
    peak_saturation,
    profile_pileup,
    qq,
    scatter,
    taxonomic_stacked_bar,
    violin,
    volcano,
)

HASH_FILE = Path(__file__).parent / "snapshot-hashes.json"
UPDATE_FLAG = "--update-hashes"


def _sha256(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def _load_hashes() -> Dict[str, str]:
    if HASH_FILE.exists():
        return json.loads(HASH_FILE.read_text())
    return {}


def _save_hashes(hashes: Dict[str, str]) -> None:
    HASH_FILE.write_text(json.dumps(hashes, indent=2, sort_keys=True) + "\n")


# `--update-hashes` is registered in conftest.py so pytest picks it up
# at collection time. Tests below read it via request.config.getoption.

# Each fixture renders a figure given a tmp_path and returns the produced
# Path. The fixture name is the stable key used in snapshot-hashes.json.
FIXTURES: Dict[str, Callable[[Path], Path]] = {}


def _register_fixture(name: str):
    def deco(fn: Callable[[Path], Path]) -> Callable[[Path], Path]:
        FIXTURES[name] = fn
        return fn

    return deco


# --------------------------------------------------------------------------
# Snapshot fixtures — each must be byte-deterministic across runs.
# --------------------------------------------------------------------------


@_register_fixture("volcano_de_basic")
def _fix_volcano(tmp: Path) -> Path:
    rng = np.random.default_rng(7)
    n = 500
    log_fc = rng.normal(scale=2.0, size=n)
    p = np.clip(np.abs(rng.normal(scale=0.15, size=n)), 1e-6, 1.0)
    return volcano(
        log_fc=log_fc,
        neg_log10_p=-np.log10(p),
        title="DE: aged vs healthy",
        out=tmp / "volcano_de_basic.png",
        labels=[f"GENE{i}" for i in range(n)],
        label_top_n=10,
    )


@_register_fixture("bar_with_ci")
def _fix_bar_ci(tmp: Path) -> Path:
    return bar(
        names=["Ctrl", "Mild", "Moderate", "Severe"],
        values=[1.0, 1.4, 2.1, 3.2],
        ci_lo=[0.8, 1.1, 1.7, 2.6],
        ci_hi=[1.2, 1.7, 2.5, 3.8],
        title="effect size by grade",
        ylabel="log2 fold change",
        out=tmp / "bar_with_ci.png",
    )


@_register_fixture("violin_three_groups")
def _fix_violin(tmp: Path) -> Path:
    rng = np.random.default_rng(0)
    data = {
        "Ctrl": rng.normal(loc=0.0, scale=1.0, size=60).tolist(),
        "Lo": rng.normal(loc=0.4, scale=1.0, size=60).tolist(),
        "Hi": rng.normal(loc=2.0, scale=1.0, size=60).tolist(),
    }
    return violin(
        data=data,
        title="expression by group",
        ylabel="log2 expr",
        out=tmp / "violin_three_groups.png",
    )


@_register_fixture("heatmap_clustered")
def _fix_heatmap(tmp: Path) -> Path:
    rng = np.random.default_rng(1)
    mat = rng.normal(size=(25, 8))
    return heatmap(
        matrix=mat,
        row_labels=[f"r{i}" for i in range(25)],
        col_labels=[f"c{i}" for i in range(8)],
        title="top 25 features × 8 samples",
        out=tmp / "heatmap_clustered.png",
        z_score_rows=True,
    )


@_register_fixture("scatter_continuous")
def _fix_scatter(tmp: Path) -> Path:
    rng = np.random.default_rng(2)
    x = rng.normal(size=300)
    y = x * 0.5 + rng.normal(scale=0.5, size=300)
    return scatter(
        x=x,
        y=y,
        title="x vs y",
        xlabel="x",
        ylabel="y",
        out=tmp / "scatter_continuous.png",
        color=y,
    )


# --------------------------------------------------------------------------
# Stage-12 fixtures (plan §S12.7) — variant + GWAS + clinical figures.
# Each fixture uses a deterministic seeded RNG so the bytes are
# byte-stable across runs on the same matplotlib + freetype combo.
# --------------------------------------------------------------------------


@_register_fixture("manhattan_three_chroms")
def _fix_manhattan(tmp: Path) -> Path:
    rng = np.random.default_rng(11)
    rows = []
    for chrom, n in [("1", 200), ("2", 150), ("3", 100)]:
        for _ in range(n):
            pos = int(rng.integers(low=1, high=200_000_000))
            pval = float(np.clip(rng.uniform(0.0, 1.0), 1e-12, 1.0))
            rows.append({"chrom": chrom, "pos": pos, "pvalue": pval})
    # Sprinkle a couple of strong hits so the plot has visible labels.
    rows.append({"chrom": "1", "pos": 50_000_000, "pvalue": 5e-9, "gene": "TOP_HIT_A"})
    rows.append({"chrom": "2", "pos": 80_000_000, "pvalue": 1e-8, "gene": "TOP_HIT_B"})
    return manhattan(
        frame={
            "chrom": [r["chrom"] for r in rows],
            "pos": [r["pos"] for r in rows],
            "pvalue": [r["pvalue"] for r in rows],
            "gene": [r.get("gene", "") for r in rows],
        },
        title="GWAS test panel",
        out=tmp / "manhattan_three_chroms.png",
    )


@_register_fixture("qq_lambda")
def _fix_qq(tmp: Path) -> Path:
    rng = np.random.default_rng(12)
    pvals = np.clip(rng.uniform(0.0, 1.0, size=500), 1e-9, 1.0)
    return qq(
        frame={"pvalue": pvals},
        title="QQ — null distribution",
        out=tmp / "qq_lambda.png",
    )


@_register_fixture("forest_three_arms")
def _fix_forest(tmp: Path) -> Path:
    return forest(
        frame={
            "label": ["Subgroup A", "Subgroup B", "Subgroup C"],
            "effect": [0.32, -0.05, 0.21],
            "ci_lo": [0.10, -0.30, -0.02],
            "ci_hi": [0.55, 0.18, 0.45],
        },
        title="Effect size by subgroup",
        out=tmp / "forest_three_arms.png",
    )


@_register_fixture("kaplan_meier_two_arms")
def _fix_km(tmp: Path) -> Path:
    rng = np.random.default_rng(13)
    times_a = rng.exponential(scale=10.0, size=40)
    times_b = rng.exponential(scale=15.0, size=40)
    events_a = rng.integers(low=0, high=2, size=40).astype(int)
    events_b = rng.integers(low=0, high=2, size=40).astype(int)
    return kaplan_meier(
        frame={
            "time": np.concatenate([times_a, times_b]),
            "event": np.concatenate([events_a, events_b]),
            "arm": ["A"] * 40 + ["B"] * 40,
        },
        title="Survival by arm",
        group_col="arm",
        out=tmp / "kaplan_meier_two_arms.png",
        show_at_risk_table=False,
    )


# --------------------------------------------------------------------------
# Stage-13 fixtures (plan §S13.7) — sequencing + compositional + spatial.
# --------------------------------------------------------------------------


@_register_fixture("profile_pileup_basic")
def _fix_profile_pileup(tmp: Path) -> Path:
    rng = np.random.default_rng(14)
    positions = np.arange(-500, 501, dtype=float)
    signal = np.exp(-(positions**2) / (2 * 150**2)) + rng.normal(scale=0.05, size=positions.size)
    return profile_pileup(
        frame={"position": positions, "signal": signal},
        title="Profile pileup — TSS-anchored",
        out=tmp / "profile_pileup_basic.png",
    )


@_register_fixture("peak_saturation_basic")
def _fix_peak_saturation(tmp: Path) -> Path:
    return peak_saturation(
        frame={
            "depth": [1e6, 5e6, 1e7, 2e7, 4e7, 8e7],
            "peaks_called": [1200, 4500, 7500, 11200, 13800, 14500],
        },
        title="Peak saturation",
        out=tmp / "peak_saturation_basic.png",
    )


@_register_fixture("taxonomic_stacked_bar_three_samples")
def _fix_tax_bar(tmp: Path) -> Path:
    samples = ["S1", "S2", "S3"]
    taxa = ["Bacteroides", "Firmicutes", "Proteobacteria", "Other"]
    return taxonomic_stacked_bar(
        frame={
            "sample": samples * len(taxa),
            "taxon": [t for t in taxa for _ in samples],
            "abundance": [
                0.40, 0.35, 0.30,
                0.30, 0.32, 0.34,
                0.20, 0.18, 0.21,
                0.10, 0.15, 0.15,
            ],
        },
        title="Gut composition — pilot cohort",
        out=tmp / "taxonomic_stacked_bar_three_samples.png",
    )


@_register_fixture("diversity_violin_two_groups")
def _fix_div_violin(tmp: Path) -> Path:
    rng = np.random.default_rng(15)
    groups = ["Healthy"] * 25 + ["Disease"] * 25
    values = np.concatenate([
        rng.normal(loc=3.2, scale=0.4, size=25),
        rng.normal(loc=2.4, scale=0.5, size=25),
    ])
    return diversity_violin(
        frame={"group": groups, "diversity": values},
        title="Shannon diversity",
        out=tmp / "diversity_violin_two_groups.png",
    )


@_register_fixture("morans_i_scatter_basic")
def _fix_morans(tmp: Path) -> Path:
    rng = np.random.default_rng(16)
    n = 80
    morans = rng.normal(loc=0.1, scale=0.2, size=n)
    pvals = np.clip(np.abs(rng.normal(scale=0.1, size=n)), 1e-9, 1.0)
    return morans_i_scatter(
        frame={
            "gene": [f"GENE{i:03d}" for i in range(n)],
            "morans_i": morans,
            "p_value": pvals,
        },
        title="Spatial autocorrelation",
        out=tmp / "morans_i_scatter_basic.png",
    )


# --------------------------------------------------------------------------
# Stage-14 fixtures (plan §S14.3 / §S14.6 — time-series).
# --------------------------------------------------------------------------


@_register_fixture("forecast_ribbon_24mo")
def _fix_forecast(tmp: Path) -> Path:
    rng = np.random.default_rng(17)
    t = np.arange(24)
    yhat = 100 + 5 * np.sin(t / 3.0) + rng.normal(scale=2.0, size=24)
    return forecast_ribbon(
        frame={
            "time": t,
            "forecast": yhat,
            "lower": yhat - 6,
            "upper": yhat + 6,
            "actual": yhat + rng.normal(scale=1.0, size=24),
        },
        title="24-month forecast",
        out=tmp / "forecast_ribbon_24mo.png",
    )


@_register_fixture("anomaly_timeline_basic")
def _fix_anomaly(tmp: Path) -> Path:
    rng = np.random.default_rng(18)
    n = 50
    values = rng.normal(loc=10.0, scale=1.0, size=n)
    flags = np.zeros(n, dtype=int)
    flags[[7, 18, 33]] = 1
    values[[7, 18, 33]] = [16.0, 4.0, 17.5]
    return anomaly_timeline(
        frame={
            "time": np.arange(n),
            "value": values,
            "is_anomaly": flags,
        },
        title="Detected anomalies",
        out=tmp / "anomaly_timeline_basic.png",
    )


# --------------------------------------------------------------------------
# Second-fixture pass — every figure type carries ≥2 fixtures (a
# "_basic"/"_v1" plus a contrasting second case) so the snapshot
# suite catches drift on either edge of the parameter envelope.
# Each second fixture varies inputs from its
# first variant: different RNG seed, different shape (e.g. group
# count, time range), or a contrasting input distribution. Together
# the pair covers more of the rendering surface than a single fixture
# would.
# --------------------------------------------------------------------------


@_register_fixture("manhattan_dense_one_chrom")
def _fix_manhattan_dense(tmp: Path) -> Path:
    """Single chromosome with a dense band of suggestive hits — exercises
    the chromosome-as-single-color path + the suggestive-threshold line."""
    rng = np.random.default_rng(21)
    n = 600
    rows = []
    for _ in range(n):
        pos = int(rng.integers(low=1, high=180_000_000))
        pval = float(np.clip(rng.uniform(0.0, 0.5), 1e-10, 1.0))
        rows.append((pos, pval))
    return manhattan(
        frame={
            "chrom": ["7"] * n,
            "pos": [r[0] for r in rows],
            "pvalue": [r[1] for r in rows],
        },
        title="GWAS — single chromosome zoom",
        out=tmp / "manhattan_dense_one_chrom.png",
        suggestive_threshold=1e-5,
    )


@_register_fixture("qq_inflated")
def _fix_qq_inflated(tmp: Path) -> Path:
    """QQ with deliberate inflation — every pvalue × 0.5 so the lambda_GC
    annotation deviates from 1.0 visibly."""
    rng = np.random.default_rng(22)
    pvals = np.clip(rng.uniform(0.0, 1.0, size=400), 1e-9, 1.0) * 0.5
    return qq(
        frame={"pvalue": pvals},
        title="QQ — inflated test statistic",
        out=tmp / "qq_inflated.png",
    )


@_register_fixture("forest_subgroups_with_overall")
def _fix_forest_overall(tmp: Path) -> Path:
    """Five subgroups + an "Overall" pooled row matching the meta-analysis
    convention — null line at 1.0 (hazard-ratio space)."""
    return forest(
        frame={
            "label": ["Age <50", "Age 50–65", "Age >65", "Male", "Female", "Overall"],
            "effect": [0.85, 0.92, 1.05, 0.88, 0.97, 0.93],
            "ci_lo": [0.62, 0.71, 0.81, 0.66, 0.75, 0.78],
            "ci_hi": [1.16, 1.18, 1.36, 1.18, 1.25, 1.10],
        },
        title="Hazard ratio by subgroup",
        out=tmp / "forest_subgroups_with_overall.png",
        null_value=1.0,
    )


@_register_fixture("kaplan_meier_no_groups")
def _fix_km_single(tmp: Path) -> Path:
    """KM with a single arm — no group_col, exercises the single-curve
    branch that the two-arm fixture skips."""
    rng = np.random.default_rng(23)
    times = rng.exponential(scale=12.0, size=80)
    events = rng.integers(low=0, high=2, size=80).astype(int)
    return kaplan_meier(
        frame={"time": times, "event": events},
        title="Overall survival",
        out=tmp / "kaplan_meier_no_groups.png",
        show_at_risk_table=False,
    )


@_register_fixture("profile_pileup_grouped")
def _fix_profile_pileup_grouped(tmp: Path) -> Path:
    """Profile pileup with a `group` column — exercises the multi-line
    overlay path the basic fixture's single-group case doesn't hit."""
    rng = np.random.default_rng(24)
    positions = np.arange(-500, 501, dtype=float)
    n = positions.size
    groups: list[str] = []
    pos: list[float] = []
    sig: list[float] = []
    for label, sd in [("H3K27me3", 100.0), ("H3K4me3", 200.0)]:
        groups.extend([label] * n)
        pos.extend(positions.tolist())
        peak = np.exp(-(positions**2) / (2 * sd**2))
        sig.extend((peak + rng.normal(scale=0.04, size=n)).tolist())
    return profile_pileup(
        frame={"position": pos, "signal": sig, "group": groups},
        title="Profile pileup — two antibodies",
        out=tmp / "profile_pileup_grouped.png",
        group_col="group",
    )


@_register_fixture("peak_saturation_two_replicates")
def _fix_peak_saturation_replicates(tmp: Path) -> Path:
    """Peak saturation with two replicates — exercises the group_col
    multi-line path the basic single-curve fixture skips."""
    return peak_saturation(
        frame={
            "depth": [1e6, 5e6, 1e7, 2e7, 4e7, 8e7] * 2,
            "peaks_called": [
                1100, 4400, 7400, 11000, 13500, 14400,
                1300, 4700, 7800, 11500, 14200, 14900,
            ],
            "group": ["rep1"] * 6 + ["rep2"] * 6,
        },
        title="Peak saturation — two replicates",
        out=tmp / "peak_saturation_two_replicates.png",
        group_col="group",
    )


@_register_fixture("taxonomic_stacked_bar_horizontal")
def _fix_tax_bar_horizontal(tmp: Path) -> Path:
    """Horizontal layout + 6 taxa — exercises the orientation flip the
    vertical default doesn't hit."""
    samples = ["S1", "S2", "S3", "S4"]
    taxa = [
        "Bacteroides", "Firmicutes", "Proteobacteria",
        "Actinobacteria", "Fusobacteria", "Other",
    ]
    abundances = [
        0.30, 0.32, 0.28, 0.34,
        0.25, 0.24, 0.27, 0.22,
        0.18, 0.20, 0.19, 0.17,
        0.12, 0.10, 0.13, 0.14,
        0.08, 0.09, 0.07, 0.08,
        0.07, 0.05, 0.06, 0.05,
    ]
    return taxonomic_stacked_bar(
        frame={
            "sample": samples * len(taxa),
            "taxon": [t for t in taxa for _ in samples],
            "abundance": abundances,
        },
        title="Composition — 4 samples × 6 taxa (horizontal)",
        out=tmp / "taxonomic_stacked_bar_horizontal.png",
        horizontal=True,
    )


@_register_fixture("diversity_violin_three_groups")
def _fix_div_violin_three(tmp: Path) -> Path:
    """Three groups instead of two — exercises the multi-group palette
    rotation."""
    rng = np.random.default_rng(25)
    groups = ["Healthy"] * 25 + ["Mild"] * 25 + ["Severe"] * 25
    values = np.concatenate([
        rng.normal(loc=3.5, scale=0.3, size=25),
        rng.normal(loc=2.9, scale=0.4, size=25),
        rng.normal(loc=2.2, scale=0.5, size=25),
    ])
    return diversity_violin(
        frame={"group": groups, "diversity": values},
        title="Shannon diversity — disease severity gradient",
        out=tmp / "diversity_violin_three_groups.png",
    )


@_register_fixture("morans_i_scatter_dense")
def _fix_morans_dense(tmp: Path) -> Path:
    """Dense Moran's I scatter — 200 genes with skewed I distribution,
    contrasts with the 80-gene basic fixture."""
    rng = np.random.default_rng(26)
    n = 200
    morans = rng.beta(2, 5, size=n) * 0.6  # skewed toward 0
    pvals = np.clip(np.abs(rng.normal(scale=0.12, size=n)), 1e-10, 1.0)
    return morans_i_scatter(
        frame={
            "gene": [f"GENE{i:04d}" for i in range(n)],
            "morans_i": morans,
            "p_value": pvals,
        },
        title="Spatial autocorrelation — dense gene panel",
        out=tmp / "morans_i_scatter_dense.png",
    )


@_register_fixture("forecast_ribbon_no_actual")
def _fix_forecast_no_actual(tmp: Path) -> Path:
    """Forecast without the `actual` overlay — the planning-mode case
    where the report is being prepared before holdout data arrives."""
    rng = np.random.default_rng(27)
    t = np.arange(36)
    yhat = 50 + 8 * np.sin(t / 4.0) + rng.normal(scale=1.5, size=36)
    return forecast_ribbon(
        frame={
            "time": t,
            "forecast": yhat,
            "lower": yhat - 4,
            "upper": yhat + 4,
        },
        title="36-month forecast (planning-only)",
        out=tmp / "forecast_ribbon_no_actual.png",
        actual_col=None,
    )


@_register_fixture("anomaly_timeline_dense")
def _fix_anomaly_dense(tmp: Path) -> Path:
    """100-point series with 8 flagged anomalies vs the basic 50-point /
    3-anomaly fixture — exercises the multi-band layout."""
    rng = np.random.default_rng(28)
    n = 100
    values = rng.normal(loc=20.0, scale=2.0, size=n)
    flags = np.zeros(n, dtype=int)
    anomaly_idx = [11, 22, 38, 47, 61, 73, 85, 94]
    for i, k in enumerate(anomaly_idx):
        flags[k] = 1
        values[k] = 30.0 if i % 2 == 0 else 10.0
    return anomaly_timeline(
        frame={
            "time": np.arange(n),
            "value": values,
            "is_anomaly": flags,
        },
        title="Detected anomalies — dense regime",
        out=tmp / "anomaly_timeline_dense.png",
    )


# --------------------------------------------------------------------------
# Per-fixture snapshot test
# --------------------------------------------------------------------------


@pytest.mark.parametrize("fixture_name", sorted(FIXTURES))
def test_snapshot_byte_stable(fixture_name: str, tmp_path: Path, request):
    """Render the fixture and compare its hash to the committed golden.
    Fails when the hash drifts; passes silently when no golden exists
    yet (initial baselining).
    """
    update = request.config.getoption(UPDATE_FLAG, default=False)
    fn = FIXTURES[fixture_name]
    rendered_png = fn(tmp_path)
    rendered_pdf = rendered_png.with_suffix(".pdf")
    assert rendered_png.exists(), f"{fixture_name}: PNG not produced"
    assert rendered_pdf.exists(), f"{fixture_name}: PDF not produced"

    h_png = _sha256(rendered_png)
    h_pdf = _sha256(rendered_pdf)

    hashes = _load_hashes()
    if update:  # pragma: no cover
        hashes[f"{fixture_name}.png"] = h_png
        hashes[f"{fixture_name}.pdf"] = h_pdf
        _save_hashes(hashes)
        return

    expected_png = hashes.get(f"{fixture_name}.png")
    expected_pdf = hashes.get(f"{fixture_name}.pdf")
    if expected_png is None or expected_pdf is None:
        pytest.skip(
            f"no golden for {fixture_name}; run with {UPDATE_FLAG} to baseline"
        )
    assert h_png == expected_png, (
        f"PNG hash drift for {fixture_name}: got {h_png}, expected {expected_png}. "
        f"Re-baseline with {UPDATE_FLAG} after reviewing the visual change."
    )
    assert h_pdf == expected_pdf, (
        f"PDF hash drift for {fixture_name}: got {h_pdf}, expected {expected_pdf}. "
        f"Re-baseline with {UPDATE_FLAG} after reviewing the visual change."
    )


def test_fixture_self_byte_determinism(tmp_path):
    """Each registered fixture, rendered twice, must produce identical
    bytes. This is the hard floor — any flake here is a determinism
    bug, not a baselining problem.
    """
    failures = []
    for name, fn in FIXTURES.items():
        a = fn(tmp_path / f"{name}_a")
        b = fn(tmp_path / f"{name}_b")
        if _sha256(a) != _sha256(b):
            failures.append(f"{name}: PNG diverged")
        a_pdf = a.with_suffix(".pdf")
        b_pdf = b.with_suffix(".pdf")
        if _sha256(a_pdf) != _sha256(b_pdf):
            failures.append(f"{name}: PDF diverged")
    assert not failures, "\n".join(failures)
