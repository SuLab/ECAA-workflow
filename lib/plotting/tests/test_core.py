"""Tests for core primitives. Run with `pytest lib/plotting/tests`.

These tests are modality-agnostic: they synthesize fixture data in-memory
rather than depending on a real h5ad or DE output.
"""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

import matplotlib
import numpy as np
import pytest

# Allow `from lib.plotting...` imports when pytest is run from the repo root
ROOT = Path(__file__).resolve().parents[3]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from lib.plotting.core import (  # noqa: E402
    THEME,
    FigureManifest,
    __version__,
    apply_theme,
    bar,
    categorical_palette,
    generate,
    glasbey20_palette,
    heatmap,
    register_alias,
    register_figure,
    savefig,
    scatter,
    seeded,
    stage_registry,
    violin,
    volcano,
    wong_palette,
)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def test_registry_register_and_iter():
    reg = stage_registry("demo")

    @register_figure(reg, "ones")
    def _ones(ctx, out):
        return out

    assert "ones" in reg
    assert reg.get("ones") is _ones
    assert list(reg) == ["ones"]


def test_register_alias_resolves_to_source_renderer():
    """Plan §S14.5 — alias_id and source_id resolve to the same fn so
    taxonomies can name either id without a duplicate decorator."""
    reg = stage_registry("demo_alias")

    @register_figure(reg, "primary")
    def _primary(ctx, out):
        return out

    register_alias(reg, "alias_a", "primary")
    register_alias(reg, "alias_b", "primary")

    assert reg.get("alias_a") is _primary
    assert reg.get("alias_b") is _primary
    # Both ids are reachable via __contains__.
    assert "primary" in reg
    assert "alias_a" in reg
    assert "alias_b" in reg


def test_register_alias_rejects_unknown_source():
    """Plan §S14.5 — aliases must point at concrete renderers; trying
    to alias an unregistered id surfaces ValueError."""
    import pytest

    reg = stage_registry("demo_alias_bad")
    with pytest.raises(ValueError, match="not yet registered"):
        register_alias(reg, "alias_x", "ghost_renderer")


def test_seeded_restores_rng_state():
    np.random.seed(1)
    before = np.random.randn(2)
    with seeded(999):
        inside = np.random.randn(2)
    after = np.random.randn(2)
    assert np.allclose(before, before)  # trivial
    # After restoring, drawing again from the parent RNG should match
    # what it would have drawn without the seeded block.
    np.random.seed(1)
    _ = np.random.randn(2)  # re-consume `before`
    after_expected = np.random.randn(2)
    assert np.allclose(after, after_expected)
    # And `inside` should match the standalone seeded draw.
    rng_ref = np.random.RandomState(999)
    inside_expected = rng_ref.randn(2)
    assert np.allclose(inside, inside_expected)


def test_savefig_writes_and_is_deterministic(tmp_path):
    import matplotlib.pyplot as plt

    def make() -> Path:
        fig, ax = plt.subplots(figsize=(3, 2))
        ax.plot([0, 1, 2], [0, 1, 4])
        return savefig(fig, tmp_path / "plot.png")

    # Determinism requires a fixed seed + metadata strip, which savefig
    # provides — repeated calls write identical bytes.
    p1 = make()
    b1 = sha256(p1)
    p2 = make()
    b2 = sha256(p2)
    assert b1 == b2
    assert p1.stat().st_size > 0


def test_violin_produces_figure(tmp_path):
    data = {
        "s1": [1.0, 2.0, 3.0, 2.5, 2.8],
        "s2": [0.5, 0.9, 1.1, 0.7, 0.6],
    }
    out = violin(data=data, title="t", ylabel="y", out=tmp_path / "v.png")
    assert out.exists() and out.stat().st_size > 0


def test_bar_produces_figure(tmp_path):
    out = bar(
        names=["a", "b", "c"], values=[1.0, 2.0, 3.0],
        title="t", ylabel="y", out=tmp_path / "b.png",
    )
    assert out.exists() and out.stat().st_size > 0


def test_scatter_produces_figure(tmp_path):
    rng = np.random.default_rng(0)
    x = rng.normal(size=200)
    y = rng.normal(size=200)
    out = scatter(x=x, y=y, title="t", xlabel="x", ylabel="y", out=tmp_path / "s.png")
    assert out.exists() and out.stat().st_size > 0


def test_volcano_labels_top_n(tmp_path):
    rng = np.random.default_rng(0)
    log_fc = rng.normal(scale=2.0, size=500)
    p = np.clip(np.abs(rng.normal(scale=0.2, size=500)), 1e-6, 1.0)
    labels = [f"g{i}" for i in range(500)]
    out = volcano(
        log_fc=log_fc,
        neg_log10_p=-np.log10(p),
        title="t",
        out=tmp_path / "vp.png",
        labels=labels,
        label_top_n=5,
    )
    assert out.exists() and out.stat().st_size > 0


def test_heatmap_produces_figure(tmp_path):
    rng = np.random.default_rng(0)
    mat = rng.normal(size=(10, 4))
    out = heatmap(
        matrix=mat,
        row_labels=[f"r{i}" for i in range(10)],
        col_labels=["c1", "c2", "c3", "c4"],
        title="h",
        out=tmp_path / "h.png",
    )
    assert out.exists() and out.stat().st_size > 0


def test_generate_dispatches_and_writes_manifest(tmp_path):
    # Pre-seed a manifest with per_sample_metrics so the qc_summary_bar
    # and per_sample_metric_bar helpers have data to render.
    outputs = tmp_path / "outputs"
    outputs.mkdir()
    manifest = {
        "per_sample_metrics": {
            "s1": {"n_cells": 100, "n_features": 20},
            "s2": {"n_cells": 150, "n_features": 22},
            "s3": {"n_cells": 90, "n_features": 18},
        }
    }
    (outputs / "manifest.json").write_text(json.dumps(manifest))
    mf = generate(
        "quality_control",
        outputs,
        required=["per_sample_metric_violin", "per_sample_metric_bar"],
    )
    assert isinstance(mf, FigureManifest)
    assert "per_sample_metric_bar" in mf.written
    # per_sample_metric_violin falls back to bar when no long-form TSV
    # is present — still counted as written.
    assert "per_sample_metric_violin" in mf.written
    # figures/manifest.json was written
    assert (outputs / "figures" / "manifest.json").exists()
    data = json.loads((outputs / "figures" / "manifest.json").read_text())
    assert data["stage_id"] == "quality_control"
    assert "per_sample_metric_bar" in data["written"]


def test_generate_reporting_required_figures(tmp_path):
    outputs = tmp_path / "reporting"
    outputs.mkdir()
    manifest = {
        "concordance_matrix": [[1.0, 0.25], [0.25, 1.0]],
        "row_labels": ["rna", "protein"],
        "col_labels": ["rna", "protein"],
        "pathway_overlap": [
            {"label": "immune", "count": 12},
            {"label": "metabolism", "count": 7},
        ],
    }
    (outputs / "manifest.json").write_text(json.dumps(manifest))
    mf = generate(
        "reporting",
        outputs,
        required=["concordance_heatmap", "pathway_overlap_bar"],
    )
    assert set(mf.written) == {"concordance_heatmap", "pathway_overlap_bar"}
    for fig in mf.written:
        assert (outputs / "figures" / f"{fig}.png").exists()
        assert (outputs / "figures" / f"{fig}.pdf").exists()


def test_generate_multi_omics_required_figures(tmp_path):
    outputs = tmp_path / "multi"
    outputs.mkdir()
    manifest = {
        "modalities": ["rnaseq", "proteomics"],
        "factor_variance": {"factor_1": 0.42, "factor_2": 0.21},
    }
    (outputs / "manifest.json").write_text(json.dumps(manifest))
    mf = generate(
        "multi_omics_integration",
        outputs,
        required=["modality_concordance_heatmap", "factor_variance_bar"],
    )
    assert set(mf.written) == {"modality_concordance_heatmap", "factor_variance_bar"}
    for fig in mf.written:
        assert (outputs / "figures" / f"{fig}.png").exists()
        assert (outputs / "figures" / f"{fig}.pdf").exists()


def test_generate_unknown_stage_is_soft_skip(tmp_path):
    outputs = tmp_path / "outputs"
    outputs.mkdir()
    mf = generate("no_such_stage_xyz", outputs)
    assert mf.written == {}
    assert "*" in mf.skipped


def test_generate_unknown_figure_id(tmp_path):
    outputs = tmp_path / "outputs"
    outputs.mkdir()
    (outputs / "manifest.json").write_text(
        json.dumps({"per_sample_metrics": {"a": {"n_cells": 1}}})
    )
    mf = generate(
        "quality_control",
        outputs,
        required=["not_a_real_figure", "per_sample_metric_bar"],
    )
    assert "not_a_real_figure" in mf.skipped
    assert "per_sample_metric_bar" in mf.written


def test_generate_missing_input_is_reported_as_skipped(tmp_path):
    outputs = tmp_path / "outputs"
    outputs.mkdir()
    # No manifest, no TSV — the violin function should raise
    # FileNotFoundError and surface as skipped.
    mf = generate("quality_control", outputs, required=["per_sample_metric_violin"])
    assert "per_sample_metric_violin" in mf.skipped


def test_generate_determinism_byte_reproducibility(tmp_path):
    outputs1 = tmp_path / "r1" / "outputs"
    outputs2 = tmp_path / "r2" / "outputs"
    for out in (outputs1, outputs2):
        out.mkdir(parents=True)
        (out / "manifest.json").write_text(
            json.dumps(
                {
                    "per_sample_metrics": {
                        "x": {"n_cells": 100},
                        "y": {"n_cells": 150},
                        "z": {"n_cells": 90},
                    }
                }
            )
        )
    mf1 = generate("quality_control", outputs1, required=["per_sample_metric_bar"])
    mf2 = generate("quality_control", outputs2, required=["per_sample_metric_bar"])
    assert mf1.written and mf2.written
    h1 = sha256(mf1.written["per_sample_metric_bar"])
    h2 = sha256(mf2.written["per_sample_metric_bar"])
    assert h1 == h2, "figure bytes must match across repeat runs"


def test_differential_expression_volcano_from_tsv(tmp_path):
    outputs = tmp_path / "outputs"
    outputs.mkdir()
    comp = outputs / "cmp_a"
    comp.mkdir()
    rng = np.random.default_rng(0)
    n = 200
    fc = rng.normal(scale=2.0, size=n)
    p = np.clip(np.abs(rng.normal(scale=0.2, size=n)), 1e-6, 1.0)
    with (comp / "de_table.tsv").open("w") as f:
        f.write("feature\tlog2FoldChange\tpvalue\n")
        for i in range(n):
            f.write(f"g{i}\t{fc[i]}\t{p[i]}\n")
    mf = generate("differential_expression", outputs, required=["volcano", "top_features_heatmap"])
    assert "volcano" in mf.written
    assert "top_features_heatmap" in mf.written


def test_batch_correction_mixing_bar(tmp_path):
    outputs = tmp_path / "outputs"
    outputs.mkdir()
    manifest = {
        "compartments": [
            {"id": "NP", "mixing_pre": 0.15, "mixing_post": 0.27},
            {"id": "all", "mixing_pre": 0.28, "mixing_post": 0.36},
        ]
    }
    (outputs / "manifest.json").write_text(json.dumps(manifest))
    mf = generate("batch_correction", outputs, required=["mixing_score_bar"])
    assert "mixing_score_bar" in mf.written


def test_dimensionality_reduction_elbow(tmp_path):
    outputs = tmp_path / "outputs"
    outputs.mkdir()
    manifest = {
        "compartments": [
            {"id": "NP", "variance_explained": [0.2, 0.15, 0.1, 0.05, 0.02]},
        ]
    }
    (outputs / "manifest.json").write_text(json.dumps(manifest))
    mf = generate("dimensionality_reduction", outputs, required=["variance_explained_elbow"])
    assert "variance_explained_elbow" in mf.written


def test_extract_writes_view_json(tmp_path):
    from lib.plotting.core import extract

    outputs = tmp_path / "outputs"
    outputs.mkdir()
    manifest = {
        "compartments": [
            {"id": "NP", "variance_explained": [0.2, 0.15, 0.1, 0.05, 0.02]},
        ]
    }
    (outputs / "manifest.json").write_text(json.dumps(manifest))
    mf = extract(
        "dimensionality_reduction",
        outputs,
        required=["variance_explained"],
    )
    assert "variance_explained" in mf.written
    payload = json.loads(
        (outputs / "view_data" / "variance_explained.json").read_text()
    )
    assert payload["stage_id"] == "dimensionality_reduction"
    assert payload["view_id"] == "variance_explained"
    assert payload["data"]["runs"][0]["cumulative"][0] > 0


def test_generate_auto_calls_extract(tmp_path):
    """generate() on a stage that registers both FIGURES and VIEWS
    should produce both figures/ and view_data/ manifests so the
    agent only has to call one entry point.
    """
    outputs = tmp_path / "outputs"
    outputs.mkdir()
    manifest = {
        "compartments": [
            {"id": "NP", "variance_explained": [0.2, 0.15, 0.1]},
        ]
    }
    (outputs / "manifest.json").write_text(json.dumps(manifest))
    mf = generate(
        "dimensionality_reduction",
        outputs,
        required=["variance_explained_elbow"],
    )
    assert "variance_explained_elbow" in mf.written
    # view_data manifest written as a side effect
    vdm = outputs / "view_data" / "manifest.json"
    assert vdm.exists()
    vd = json.loads(vdm.read_text())
    assert vd["stage_id"] == "dimensionality_reduction"
    assert "variance_explained" in vd["written"]


def test_extract_unknown_stage_returns_skipped(tmp_path):
    from lib.plotting.core import extract

    outputs = tmp_path / "outputs"
    outputs.mkdir()
    mf = extract("no_such_xyz", outputs)
    assert mf.written == {}
    assert "*" in mf.skipped


def test_extract_determinism(tmp_path):
    """Same inputs + seed must produce identical view_data JSON bytes
    across runs — mirrors the figure determinism contract.
    """
    from lib.plotting.core import extract

    rows = []
    rng = np.random.default_rng(42)
    for i in range(200):
        rows.append(
            ("g{}".format(i), float(rng.normal(scale=2)), float(rng.uniform(1e-6, 1.0)))
        )
    hashes = []
    for r in range(2):
        outputs = tmp_path / f"run{r}" / "outputs"
        comp = outputs / "comparison_a"
        comp.mkdir(parents=True)
        with (comp / "de_table.tsv").open("w") as f:
            f.write("feature\tlog2FoldChange\tpvalue\n")
            for feat, fc, p in rows:
                f.write(f"{feat}\t{fc}\t{p}\n")
        mf = extract("differential_expression", outputs, required=["volcano"])
        assert "volcano" in mf.written
        data = (outputs / "view_data" / "volcano.json").read_text()
        hashes.append(hashlib.sha256(data.encode()).hexdigest())
    assert hashes[0] == hashes[1], "view_data payload must be byte-reproducible"


# ---------------------------------------------------------------------------
# Phase A: theme baseline + dual-format + palette + provenance footer
# ---------------------------------------------------------------------------


def test_theme_loaded_with_expected_keys():
    """theme.json ships next to core.py and is loaded on import."""
    assert THEME["schema_version"] == 1
    assert "fonts" in THEME and "palette" in THEME and "output" in THEME
    assert THEME["output"]["png_dpi"] >= 300
    assert "pdf" in [f.lower() for f in THEME["output"]["formats"]]


def test_apply_theme_sets_rcparams():
    apply_theme()
    assert matplotlib.rcParams["font.size"] == THEME["fonts"]["body_pt"]
    assert matplotlib.rcParams["pdf.fonttype"] == 42
    assert matplotlib.rcParams["axes.spines.top"] is False


def test_wong_palette_is_eight_colors_and_starts_with_black():
    pal = wong_palette()
    assert len(pal) == 8
    assert pal[0] == "#000000"


def test_categorical_palette_returns_wong_for_small_n():
    pal = categorical_palette(5)
    assert pal == list(wong_palette())[:5]


def test_categorical_palette_extends_to_glasbey_at_high_n():
    pal = categorical_palette(15)
    assert len(pal) == 15
    assert pal[:8] == list(wong_palette())
    assert pal[8] == glasbey20_palette()[8]


def test_categorical_palette_warns_above_twenty():
    with pytest.warns(UserWarning, match="exceeds 20 colors"):
        pal = categorical_palette(25, name="test_high_card_unique")
    assert len(pal) == 25  # cycles glasbey20 even with the warning


def test_savefig_writes_both_png_and_pdf_by_default(tmp_path):
    """When theme.output.formats lists ['png', 'pdf'] (the default),
    savefig writes both alongside the primary path.
    """
    import matplotlib.pyplot as plt
    fig, ax = plt.subplots(figsize=(3, 2))
    ax.plot([0, 1, 2], [1, 4, 2])
    primary = savefig(fig, tmp_path / "p.png", stage_id="test")
    assert primary == tmp_path / "p.png"
    assert (tmp_path / "p.png").exists()
    assert (tmp_path / "p.png").stat().st_size > 0
    assert (tmp_path / "p.pdf").exists()
    assert (tmp_path / "p.pdf").stat().st_size > 0


def test_savefig_png_byte_determinism_across_runs(tmp_path):
    import matplotlib.pyplot as plt

    def make(out_path: Path) -> Path:
        fig, ax = plt.subplots(figsize=(3, 2))
        ax.plot([0, 1, 2], [0, 1, 4])
        return savefig(fig, out_path, stage_id="det")

    p1 = make(tmp_path / "a.png")
    p2 = make(tmp_path / "b.png")
    assert sha256(p1) == sha256(p2)


def test_savefig_pdf_byte_determinism_across_runs(tmp_path):
    """PDF output must be byte-stable too — that's the publication
    delivery format and the contract is the same.
    """
    import matplotlib.pyplot as plt

    def make(stem: str) -> Path:
        fig, ax = plt.subplots(figsize=(3, 2))
        ax.plot([0, 1, 2], [0, 1, 4])
        savefig(fig, tmp_path / f"{stem}.png", stage_id="det")
        return tmp_path / f"{stem}.pdf"

    p1 = make("a")
    p2 = make("b")
    assert p1.exists() and p2.exists()
    assert sha256(p1) == sha256(p2)


def test_savefig_explicit_formats_skips_pdf(tmp_path):
    import matplotlib.pyplot as plt
    fig, _ = plt.subplots(figsize=(3, 2))
    primary = savefig(fig, tmp_path / "only.png", formats=["png"], stage_id="t")
    assert primary == tmp_path / "only.png"
    assert (tmp_path / "only.png").exists()
    assert not (tmp_path / "only.pdf").exists()


def test_provenance_footer_text_uses_module_version(monkeypatch):
    """The footer always carries the loaded library version. With no
    SWFC_PACKAGE_ID/SWFC_GIT_SHA in env, falls back to 'unknown' tokens
    so test bytes stay reproducible.
    """
    from lib.plotting.core import _provenance_text

    monkeypatch.delenv("SWFC_PACKAGE_ID", raising=False)
    monkeypatch.delenv("SWFC_GIT_SHA", raising=False)
    text = _provenance_text("clustering")
    assert __version__ in text
    assert "clustering" in text
    assert "unknown" in text


def test_provenance_footer_includes_env_values_when_set(monkeypatch):
    from lib.plotting.core import _provenance_text

    monkeypatch.setenv("SWFC_PACKAGE_ID", "364849f9")
    monkeypatch.setenv("SWFC_GIT_SHA", "deadbeef1234567")
    text = _provenance_text("differential_expression")
    assert "364849f9" in text
    assert "deadbee" in text  # 7-char short sha


def test_volcano_renders_top_n_labels_and_counts(tmp_path):
    """Phase B contract: volcano colors by direction × significance,
    annotates up/down counts, n_total, and labels top-N.
    """
    rng = np.random.default_rng(7)
    n = 300
    log_fc = rng.normal(scale=2.0, size=n)
    p = np.clip(np.abs(rng.normal(scale=0.2, size=n)), 1e-6, 1.0)
    out = volcano(
        log_fc=log_fc,
        neg_log10_p=-np.log10(p),
        title="DE: condition vs control",
        out=tmp_path / "v.png",
        labels=[f"g{i}" for i in range(n)],
        label_top_n=10,
    )
    assert out.exists() and out.stat().st_size > 0
    # PDF sibling produced
    assert (tmp_path / "v.pdf").exists()


def test_volcano_byte_determinism_includes_labels(tmp_path):
    """Top-N label placement uses a greedy algorithm that depends only
    on input order + theme — must be byte-stable across runs.
    """
    rng = np.random.default_rng(0)
    n = 200
    log_fc = rng.normal(scale=2.0, size=n)
    p = np.clip(np.abs(rng.normal(scale=0.2, size=n)), 1e-6, 1.0)
    labels = [f"g{i}" for i in range(n)]

    def make(stem: str) -> Path:
        return volcano(
            log_fc=log_fc,
            neg_log10_p=-np.log10(p),
            title="t",
            out=tmp_path / f"{stem}.png",
            labels=labels,
            label_top_n=8,
        )

    p1 = make("v1")
    p2 = make("v2")
    assert sha256(p1) == sha256(p2)


# ---------------------------------------------------------------------------
# Phase B3-B5: heatmap dendrograms, violin sig markers, bar CI
# ---------------------------------------------------------------------------


def test_violin_with_sig_markers(tmp_path):
    """violin() draws pairwise Mann-Whitney brackets when groups ≤ 5."""
    rng = np.random.default_rng(0)
    data = {
        "control": rng.normal(loc=0, scale=1, size=40).tolist(),
        "treated_lo": rng.normal(loc=0.3, scale=1, size=40).tolist(),
        "treated_hi": rng.normal(loc=2.5, scale=1, size=40).tolist(),
    }
    out = violin(data=data, title="violin sig", ylabel="value", out=tmp_path / "v.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "v.pdf").exists()


def test_violin_byte_determinism_with_jitter(tmp_path):
    """Jitter is seeded by stage+figure_id so determinism still holds."""
    rng = np.random.default_rng(0)
    data = {"a": rng.normal(size=30).tolist(), "b": rng.normal(loc=1, size=30).tolist()}

    def make(stem: str) -> Path:
        return violin(data=data, title="t", ylabel="y", out=tmp_path / f"{stem}.png")

    p1 = make("a")
    p2 = make("b")
    assert sha256(p1) == sha256(p2)


def test_bar_with_ci_error_bars(tmp_path):
    """bar() draws asymmetric error bars when ci_lo + ci_hi provided."""
    out = bar(
        names=["A", "B", "C"],
        values=[1.0, 2.5, 1.8],
        ci_lo=[0.5, 2.0, 1.3],
        ci_hi=[1.5, 3.0, 2.4],
        title="bar with 95% CI",
        ylabel="effect",
        out=tmp_path / "b.png",
    )
    assert out.exists() and out.stat().st_size > 0


def test_bar_horizontal_at_high_n(tmp_path):
    names = [f"g{i}" for i in range(15)]
    out = bar(names=names, values=list(range(15)), title="t", ylabel="y",
              out=tmp_path / "b.png")
    assert out.exists() and out.stat().st_size > 0


def test_heatmap_with_dendrograms(tmp_path):
    """heatmap() auto-clusters rows + columns when small enough."""
    rng = np.random.default_rng(1)
    mat = rng.normal(size=(20, 8))
    out = heatmap(
        matrix=mat,
        row_labels=[f"r{i}" for i in range(20)],
        col_labels=[f"c{i}" for i in range(8)],
        title="heatmap dendrogram",
        out=tmp_path / "h.png",
        z_score_rows=True,
    )
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "h.pdf").exists()


def test_heatmap_clustering_disable(tmp_path):
    rng = np.random.default_rng(0)
    mat = rng.normal(size=(10, 4))
    out = heatmap(
        matrix=mat,
        row_labels=[f"r{i}" for i in range(10)],
        col_labels=["c1", "c2", "c3", "c4"],
        title="h",
        out=tmp_path / "h.png",
        cluster_rows=False,
        cluster_cols=False,
    )
    assert out.exists() and out.stat().st_size > 0


def test_heatmap_byte_determinism_with_clustering(tmp_path):
    rng = np.random.default_rng(0)
    mat = rng.normal(size=(15, 6))

    def make(stem: str) -> Path:
        return heatmap(
            matrix=mat,
            row_labels=[f"r{i}" for i in range(15)],
            col_labels=[f"c{i}" for i in range(6)],
            title="t",
            out=tmp_path / f"{stem}.png",
        )

    p1 = make("a")
    p2 = make("b")
    assert sha256(p1) == sha256(p2)


def test_significance_marker_thresholds():
    from lib.plotting.core import _significance_marker
    assert _significance_marker(0.0001) == "***"
    assert _significance_marker(0.005) == "**"
    assert _significance_marker(0.03) == "*"
    assert _significance_marker(0.5) == "ns"


# ---------------------------------------------------------------------------
# Phase E2: journal preset profiles
# ---------------------------------------------------------------------------


def test_presets_shipped():
    from lib.plotting.core import available_presets

    presets = available_presets()
    assert "nature" in presets
    assert "cell" in presets
    assert "science" in presets
    assert "bioinformatics" in presets


def test_load_preset_overlays_theme():
    from lib.plotting.core import load_preset

    nature = load_preset("nature")
    # Nature uses 7pt body
    assert nature["fonts"]["body_pt"] == 7
    # _description / _preset_name annotation keys are stripped
    assert "_description" not in nature
    # Inherits palette from base theme
    assert nature["palette"]["categorical_8"] == "wong"


def test_load_preset_unknown_raises():
    from lib.plotting.core import load_preset

    with pytest.raises(FileNotFoundError):
        load_preset("not_a_real_journal")


def test_use_preset_round_trip():
    """use_preset() switches the module-level THEME and re-applies
    rcParams, then restoring via apply_theme() with the original works.
    """
    from lib.plotting import core

    original = dict(core.THEME)
    try:
        core.use_preset("nature")
        assert core.THEME["fonts"]["body_pt"] == 7
        assert matplotlib.rcParams["font.size"] == 7
    finally:
        core.THEME = original
        core.apply_theme(original)


# ---------------------------------------------------------------------------
# Phase E3: external-tool delegation registry
# ---------------------------------------------------------------------------


def test_register_delegate_round_trip(tmp_path):
    from lib.plotting import delegation

    @delegation.register_delegate(
        "test_stage_xyz", "fake_fig", tool="fake_tool",
        deterministic=True, requires=[],
    )
    def _fake(ctx, out):
        return None

    spec = delegation.lookup("test_stage_xyz", "fake_fig")
    assert spec is not None
    assert spec.tool == "fake_tool"
    assert spec.deterministic
    assert delegation.has_required_modules(spec)


def test_delegate_required_modules_missing():
    from lib.plotting import delegation

    @delegation.register_delegate(
        "test_stage_missing", "fig", tool="tool_x",
        deterministic=True, requires=["definitely_not_a_real_module_xyz"],
    )
    def _fake(ctx, out):
        return None

    spec = delegation.lookup("test_stage_missing", "fig")
    assert not delegation.has_required_modules(spec)


# ---------------------------------------------------------------------------
# Phase D2: auto-quality scorer
# ---------------------------------------------------------------------------


def test_quality_scorer_passes_clean_figure(tmp_path):
    """A vanilla figure produced by the library should pass the scorer
    file-budget + (optionally) PNG resolution checks.
    """
    from lib.plotting.core import bar
    from lib.plotting.quality_scorer import score_paths

    bar(
        names=["a", "b", "c"],
        values=[1.0, 2.0, 3.0],
        title="t",
        ylabel="y",
        out=tmp_path / "ok.png",
    )
    report = score_paths([tmp_path / "ok.png", tmp_path / "ok.pdf"])
    assert report["passed"], json.dumps(report, indent=2)
    assert report["n_files"] == 2


def test_quality_scorer_flags_oversize(tmp_path):
    """A 10MB PDF (over the 5MB ceiling) fails the file-budget check."""
    from lib.plotting.quality_scorer import score_paths

    huge = tmp_path / "huge.pdf"
    huge.write_bytes(b"X" * (6 * 1024 * 1024))
    report = score_paths([huge])
    assert not report["passed"]
    failed = [c for c in report["files"][0]["checks"] if not c["passed"]]
    assert any(c["name"] == "file_budget" for c in failed)


def test_generate_records_format_siblings_in_manifest(tmp_path):
    """generate() should list both PNG and PDF paths in
    FigureManifest.formats so the validator and RO-Crate manifest can
    publish both formats, not just the primary.
    """
    outputs = tmp_path / "outputs"
    outputs.mkdir()
    (outputs / "manifest.json").write_text(
        json.dumps(
            {
                "per_sample_metrics": {
                    "a": {"n_cells": 1},
                    "b": {"n_cells": 2},
                }
            }
        )
    )
    mf = generate("quality_control", outputs, required=["per_sample_metric_bar"])
    assert "per_sample_metric_bar" in mf.written
    siblings = mf.formats.get("per_sample_metric_bar") or []
    suffixes = sorted({p.suffix.lower() for p in siblings})
    assert ".png" in suffixes
    assert ".pdf" in suffixes


# ---------------------------------------------------------------------------
# Phase F (plan §S12.1): variant + GWAS primitives
# Tests for manhattan, qq, miami, locus_zoom, credible_set_track,
# coloc_pp_panel, forest. Each renders into a small synthetic frame
# and confirms the dual-format PNG + PDF write succeeds. Pandas isn't
# strictly required — the helpers accept any dict-like, but where pandas
# is installed we test the DataFrame path too so the public contract is
# covered.
# ---------------------------------------------------------------------------


def _has_pandas() -> bool:
    try:
        import pandas  # noqa: F401
        return True
    except ImportError:
        return False


def test_manhattan_renders_with_dict_and_pandas(tmp_path):
    from lib.plotting.core import manhattan

    rng = np.random.default_rng(7)
    n = 600
    chrom = np.repeat([str(i) for i in range(1, 7)], n // 6)
    pos = rng.integers(low=1, high=10_000_000, size=n)
    pvalue = np.clip(np.abs(rng.normal(scale=0.15, size=n)), 1e-9, 1.0)
    gene = [f"G{i}" for i in range(n)]
    frame = {"chrom": chrom, "pos": pos, "pvalue": pvalue, "gene": gene}
    out = manhattan(frame, title="GWAS smoke", out=tmp_path / "m.png", label_top_n=3)
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "m.pdf").exists()
    if _has_pandas():
        import pandas as pd
        df = pd.DataFrame(frame)
        out2 = manhattan(df, title="GWAS smoke", out=tmp_path / "m_df.png")
        assert out2.exists() and out2.stat().st_size > 0


def test_manhattan_accepts_pretransformed_neg_log10_p(tmp_path):
    from lib.plotting.core import manhattan

    frame = {
        "chrom": ["1", "1", "2", "X"],
        "pos": [100_000, 200_000, 50_000, 75_000],
        "neg_log10_p": [2.0, 5.0, 8.0, 1.0],
    }
    out = manhattan(frame, title="t", out=tmp_path / "m.png")
    assert out.exists() and out.stat().st_size > 0


def test_qq_lambda_gc_annotated(tmp_path):
    from lib.plotting.core import qq

    rng = np.random.default_rng(0)
    pvalue = np.clip(np.abs(rng.uniform(0, 1, size=300)), 1e-6, 1.0)
    frame = {"pvalue": pvalue}
    out = qq(frame, title="QQ smoke", out=tmp_path / "q.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "q.pdf").exists()


def test_miami_renders_two_traits(tmp_path):
    from lib.plotting.core import miami

    rng = np.random.default_rng(0)
    n = 200
    chrom = np.repeat([str(i) for i in range(1, 5)], n // 4)
    pos = rng.integers(1, 10_000_000, size=n)
    p_top = np.clip(np.abs(rng.normal(scale=0.15, size=n)), 1e-9, 1.0)
    p_bot = np.clip(np.abs(rng.normal(scale=0.18, size=n)), 1e-9, 1.0)
    top = {"chrom": chrom, "pos": pos, "pvalue": p_top}
    bot = {"chrom": chrom, "pos": pos, "pvalue": p_bot}
    out = miami(top, bot, title="case vs ctrl",
                top_label="case", bottom_label="ctrl",
                out=tmp_path / "miami.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "miami.pdf").exists()


def test_locus_zoom_with_ld_and_lead(tmp_path):
    from lib.plotting.core import locus_zoom

    rng = np.random.default_rng(0)
    n = 80
    pos = np.sort(rng.integers(1_000_000, 1_500_000, size=n))
    nlog = -np.log10(np.clip(np.abs(rng.normal(scale=0.1, size=n)), 1e-12, 1.0))
    ld = rng.uniform(0, 1, size=n)
    rsid = [f"rs{i}" for i in range(n)]
    frame = {"pos": pos, "neg_log10_p": nlog, "ld": ld, "rsid": rsid}
    out = locus_zoom(frame, title="locus", out=tmp_path / "lz.png")
    assert out.exists() and out.stat().st_size > 0


def test_locus_zoom_without_ld(tmp_path):
    from lib.plotting.core import locus_zoom

    rng = np.random.default_rng(0)
    pos = np.arange(50)
    nlog = rng.uniform(0, 8, size=50)
    out = locus_zoom({"pos": pos, "neg_log10_p": nlog},
                     title="locus", out=tmp_path / "lz.png")
    assert out.exists() and out.stat().st_size > 0


def test_credible_set_track_computes_default_set(tmp_path):
    from lib.plotting.core import credible_set_track

    rng = np.random.default_rng(0)
    pos = np.arange(20)
    post = rng.dirichlet(np.ones(20))
    frame = {"pos": pos, "posterior": post}
    out = credible_set_track(frame, title="fine-mapping",
                             out=tmp_path / "cs.png")
    assert out.exists() and out.stat().st_size > 0


def test_credible_set_track_explicit_set_column(tmp_path):
    from lib.plotting.core import credible_set_track

    frame = {
        "pos": [1, 2, 3, 4, 5],
        "posterior": [0.5, 0.3, 0.1, 0.05, 0.05],
        "credible_set": [True, True, False, False, False],
    }
    out = credible_set_track(frame, title="cs",
                             out=tmp_path / "cs2.png")
    assert out.exists() and out.stat().st_size > 0


def test_coloc_pp_panel_renders(tmp_path):
    from lib.plotting.core import coloc_pp_panel

    frame = {
        "region": ["geneA", "geneB", "geneC"],
        "pp_h0": [0.05, 0.10, 0.02],
        "pp_h1": [0.10, 0.20, 0.08],
        "pp_h2": [0.05, 0.10, 0.05],
        "pp_h3": [0.20, 0.40, 0.15],
        "pp_h4": [0.60, 0.20, 0.70],
    }
    out = coloc_pp_panel(frame, title="coloc", out=tmp_path / "coloc.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "coloc.pdf").exists()


def test_forest_with_weights(tmp_path):
    from lib.plotting.core import forest

    frame = {
        "label": ["Cohort A", "Cohort B", "Cohort C", "Meta"],
        "effect": [0.4, -0.1, 0.7, 0.35],
        "ci_lo":  [0.2, -0.5, 0.3, 0.20],
        "ci_hi":  [0.6,  0.3, 1.1, 0.50],
        "weight": [1.0, 0.8, 0.5, 1.0],
    }
    out = forest(frame, title="meta-analysis",
                 out=tmp_path / "forest.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "forest.pdf").exists()


def test_forest_uses_log_or_null_value(tmp_path):
    from lib.plotting.core import forest

    frame = {
        "label": ["A", "B"],
        "effect": [1.5, 0.7],
        "ci_lo":  [1.1, 0.4],
        "ci_hi":  [2.0, 1.2],
    }
    out = forest(frame, title="OR", null_value=1.0,
                 xlabel="odds ratio (95% CI)",
                 out=tmp_path / "forest_or.png")
    assert out.exists() and out.stat().st_size > 0


def test_phase_f_primitives_missing_column_raises_value_error(tmp_path):
    """Each primitive surfaces missing required columns as ValueError —
    the dispatcher catches that and reports it as a per-figure skip
    instead of killing the whole stage.
    """
    from lib.plotting.core import (coloc_pp_panel, credible_set_track,
                                   forest, locus_zoom, manhattan, qq)

    with pytest.raises(ValueError, match="chrom"):
        manhattan({"pos": [1], "pvalue": [0.1]},
                  title="t", out=tmp_path / "x.png")
    with pytest.raises(ValueError):
        qq({}, title="t", out=tmp_path / "x.png")
    with pytest.raises(ValueError):
        locus_zoom({"pos": [1]},
                   title="t", out=tmp_path / "x.png")
    with pytest.raises(ValueError, match="posterior"):
        credible_set_track({"pos": [1]},
                           title="t", out=tmp_path / "x.png")
    with pytest.raises(ValueError):
        coloc_pp_panel({"region": ["a"], "pp_h0": [0.1]},
                       title="t", out=tmp_path / "x.png")
    with pytest.raises(ValueError):
        forest({"label": ["a"], "effect": [0.5]},
               title="t", out=tmp_path / "x.png")


def test_phase_f_byte_determinism_manhattan(tmp_path):
    """Manhattan output must be byte-stable for the same input frame."""
    from lib.plotting.core import manhattan

    rng = np.random.default_rng(0)
    n = 300
    chrom = np.repeat([str(i) for i in range(1, 4)], n // 3)
    pos = np.arange(n) * 1000
    pvalue = np.clip(np.abs(rng.normal(scale=0.1, size=n)), 1e-9, 1.0)
    frame = {"chrom": chrom, "pos": pos, "pvalue": pvalue,
             "gene": [f"g{i}" for i in range(n)]}

    def make(stem: str) -> Path:
        return manhattan(frame, title="det", out=tmp_path / f"{stem}.png",
                         label_top_n=5)

    p1 = make("a")
    p2 = make("b")
    assert sha256(p1) == sha256(p2)


# ---------------------------------------------------------------------------
# Phase G (plan §S12.4-S12.6): clinical statistical figures.
# Tests for kaplan_meier, consort_diagram, cumulative_incidence, spaghetti,
# adverse_event_bar. Each renders into a small synthetic frame and confirms
# the dual-format PNG + PDF write succeeds; missing-column ValueError tests
# catch the dispatcher's per-figure skip path.
# ---------------------------------------------------------------------------


def test_kaplan_meier_renders_with_groups(tmp_path):
    from lib.plotting.core import kaplan_meier

    rng = np.random.default_rng(0)
    n = 80
    time = rng.uniform(0.5, 30.0, size=n)
    event = rng.integers(0, 2, size=n)
    group = ["A" if i % 2 == 0 else "B" for i in range(n)]
    frame = {"time": time, "event": event, "arm": group}
    out = kaplan_meier(frame, title="OS by arm",
                       out=tmp_path / "km.png",
                       group_col="arm")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "km.pdf").exists()
    if _has_pandas():
        import pandas as pd
        df = pd.DataFrame(frame)
        out2 = kaplan_meier(df, title="OS",
                            out=tmp_path / "km_df.png",
                            group_col="arm")
        assert out2.exists() and out2.stat().st_size > 0


def test_kaplan_meier_no_groups_no_at_risk(tmp_path):
    from lib.plotting.core import kaplan_meier

    frame = {"time": [1.0, 2.0, 3.0, 4.0, 5.0],
             "event": [1, 0, 1, 1, 0]}
    out = kaplan_meier(frame, title="single curve",
                       out=tmp_path / "km1.png",
                       show_at_risk_table=False)
    assert out.exists() and out.stat().st_size > 0


def test_kaplan_meier_missing_column_raises(tmp_path):
    from lib.plotting.core import kaplan_meier

    with pytest.raises(ValueError, match="time"):
        kaplan_meier({"event": [1, 0]},
                     title="t", out=tmp_path / "x.png")
    with pytest.raises(ValueError, match="event"):
        kaplan_meier({"time": [1.0, 2.0]},
                     title="t", out=tmp_path / "x.png")


def test_consort_diagram_renders_with_exclusions(tmp_path):
    from lib.plotting.core import consort_diagram

    flow = {
        "enrolled": 500,
        "enrolled_excluded": "120 not eligible",
        "randomized": 380,
        "randomized_excluded": "10 declined",
        "allocated": 370,
        "followed_up": 350,
        "followed_up_excluded": "20 lost to follow-up",
        "analyzed": 340,
    }
    out = consort_diagram(flow, title="CONSORT",
                          out=tmp_path / "consort.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "consort.pdf").exists()


def test_consort_diagram_missing_required_key_raises(tmp_path):
    from lib.plotting.core import consort_diagram

    with pytest.raises(ValueError, match="randomized"):
        consort_diagram(
            {"enrolled": 100, "allocated": 90, "followed_up": 80,
             "analyzed": 75},
            title="t", out=tmp_path / "x.png",
        )


def test_cumulative_incidence_renders(tmp_path):
    from lib.plotting.core import cumulative_incidence

    rng = np.random.default_rng(0)
    n = 60
    time = rng.uniform(0.1, 10.0, size=n)
    # 0 = censor, 1 = cause-A, 2 = cause-B (competing).
    event = rng.integers(0, 3, size=n)
    frame = {"time": time, "event": event}
    out = cumulative_incidence(frame, title="CIF",
                               out=tmp_path / "cif.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "cif.pdf").exists()


def test_cumulative_incidence_with_groups(tmp_path):
    from lib.plotting.core import cumulative_incidence

    rng = np.random.default_rng(1)
    n = 60
    time = rng.uniform(0.1, 10.0, size=n)
    event = rng.integers(0, 3, size=n)
    group = ["arm1" if i % 2 == 0 else "arm2" for i in range(n)]
    frame = {"time": time, "event": event, "arm": group}
    out = cumulative_incidence(frame, title="CIF by arm",
                               out=tmp_path / "cif_g.png",
                               group_col="arm")
    assert out.exists() and out.stat().st_size > 0


def test_cumulative_incidence_missing_column_raises(tmp_path):
    from lib.plotting.core import cumulative_incidence

    with pytest.raises(ValueError, match="time"):
        cumulative_incidence({"event": [1, 0, 2]},
                             title="t", out=tmp_path / "x.png")


def test_cumulative_incidence_no_events_handled(tmp_path):
    from lib.plotting.core import cumulative_incidence

    frame = {"time": [1.0, 2.0, 3.0], "event": [0, 0, 0]}
    out = cumulative_incidence(frame, title="all censored",
                               out=tmp_path / "cif_e.png")
    assert out.exists() and out.stat().st_size > 0


def test_spaghetti_renders_with_groups(tmp_path):
    from lib.plotting.core import spaghetti

    rng = np.random.default_rng(0)
    subjects = list(range(8))
    rows = []
    for sid in subjects:
        for ti in range(5):
            rows.append({
                "id": sid,
                "time": float(ti),
                "value": float(rng.normal(loc=ti * 0.3, scale=0.5)),
                "arm": "A" if sid % 2 == 0 else "B",
            })
    frame = {
        "id": [r["id"] for r in rows],
        "time": [r["time"] for r in rows],
        "value": [r["value"] for r in rows],
        "arm": [r["arm"] for r in rows],
    }
    out = spaghetti(frame, title="biomarker over time",
                    out=tmp_path / "spag.png",
                    group_col="arm")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "spag.pdf").exists()


def test_spaghetti_no_groups_no_mean(tmp_path):
    from lib.plotting.core import spaghetti

    frame = {
        "id": [1, 1, 2, 2],
        "time": [0.0, 1.0, 0.0, 1.0],
        "value": [0.5, 0.7, 0.3, 0.4],
    }
    out = spaghetti(frame, title="solo", out=tmp_path / "spag1.png",
                    show_mean=False)
    assert out.exists() and out.stat().st_size > 0


def test_spaghetti_missing_column_raises(tmp_path):
    from lib.plotting.core import spaghetti

    with pytest.raises(ValueError, match="value"):
        spaghetti({"id": [1, 1], "time": [0.0, 1.0]},
                  title="t", out=tmp_path / "x.png")


def test_adverse_event_bar_renders_with_severity(tmp_path):
    from lib.plotting.core import adverse_event_bar

    frame = {
        "term": ["Headache", "Headache", "Headache",
                 "Nausea", "Nausea",
                 "Fatigue",
                 "Rash"],
        "severity": ["Grade 1", "Grade 2", "Grade 3+",
                     "Grade 1", "Grade 2",
                     "Grade 1",
                     "Grade 2"],
        "count": [40, 12, 3, 20, 5, 30, 6],
    }
    out = adverse_event_bar(frame, title="AEs",
                            out=tmp_path / "ae.png",
                            severity_col="severity",
                            top_n=10)
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "ae.pdf").exists()


def test_adverse_event_bar_renders_no_severity_top_n(tmp_path):
    from lib.plotting.core import adverse_event_bar

    rng = np.random.default_rng(0)
    terms = [f"AE_{i}" for i in range(15)]
    counts = rng.integers(1, 50, size=15).astype(int)
    frame = {"term": terms, "count": counts}
    out = adverse_event_bar(frame, title="AE freq",
                            out=tmp_path / "ae_top.png",
                            top_n=5)
    assert out.exists() and out.stat().st_size > 0


def test_adverse_event_bar_missing_column_raises(tmp_path):
    from lib.plotting.core import adverse_event_bar

    with pytest.raises(ValueError, match="count"):
        adverse_event_bar({"term": ["A", "B"]},
                          title="t", out=tmp_path / "x.png")


# ---------------------------------------------------------------------------
# Phase H (plan §S13.1-§S13.3): sequencing/ChIP/ATAC, long-read RNA-seq,
# proteomics primitives. Each primitive gets a round-trip render test plus
# a missing-required-column ValueError test so the dispatcher's per-figure
# skip path stays exercised.
# ---------------------------------------------------------------------------


def test_profile_pileup_renders_with_groups(tmp_path):
    from lib.plotting.core import profile_pileup

    rng = np.random.default_rng(0)
    positions = np.tile(np.linspace(-500, 500, 21), 4)
    signal = rng.uniform(0.5, 5.0, size=positions.size)
    group = np.repeat(["A", "B"], positions.size // 2)
    frame = {"position": positions, "signal": signal, "group": group}
    out = profile_pileup(frame, title="ChIP pileup",
                        out=tmp_path / "pileup.png",
                        group_col="group")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "pileup.pdf").exists()


def test_profile_pileup_missing_column_raises(tmp_path):
    from lib.plotting.core import profile_pileup

    with pytest.raises(ValueError, match="signal"):
        profile_pileup({"position": [1, 2, 3]},
                       title="t", out=tmp_path / "x.png")


def test_coverage_track_renders_with_region(tmp_path):
    from lib.plotting.core import coverage_track

    rng = np.random.default_rng(1)
    n = 200
    chrom = np.repeat(["chr1", "chr2"], n // 2)
    pos = np.concatenate([np.arange(n // 2), np.arange(n // 2)])
    depth = rng.integers(0, 50, size=n).astype(float)
    frame = {"chrom": chrom, "pos": pos, "depth": depth}
    out = coverage_track(frame, title="cov",
                         out=tmp_path / "cov.png",
                         region=("chr1", 0, 80))
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "cov.pdf").exists()


def test_coverage_track_missing_column_raises(tmp_path):
    from lib.plotting.core import coverage_track

    with pytest.raises(ValueError, match="depth"):
        coverage_track({"chrom": ["chr1"], "pos": [1]},
                       title="t", out=tmp_path / "x.png")


def test_peak_saturation_renders(tmp_path):
    from lib.plotting.core import peak_saturation

    frame = {
        "depth": [1e5, 5e5, 1e6, 5e6, 1e7],
        "peaks_called": [1200, 4500, 7800, 11000, 11800],
    }
    out = peak_saturation(frame, title="saturation",
                          out=tmp_path / "sat.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "sat.pdf").exists()


def test_peak_saturation_missing_column_raises(tmp_path):
    from lib.plotting.core import peak_saturation

    with pytest.raises(ValueError, match="peaks_called"):
        peak_saturation({"depth": [1e5, 1e6]},
                        title="t", out=tmp_path / "x.png")


def test_isoform_structure_renders_packed_form(tmp_path):
    from lib.plotting.core import isoform_structure

    frame = {
        "transcript": ["tx1", "tx2"],
        "exon_starts": [[100, 300, 600], [120, 400]],
        "exon_ends":   [[200, 500, 800], [180, 700]],
        "strand": ["+", "-"],
    }
    out = isoform_structure(frame, title="isoform models",
                            out=tmp_path / "iso.png",
                            strand_col="strand")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "iso.pdf").exists()


def test_isoform_structure_missing_column_raises(tmp_path):
    from lib.plotting.core import isoform_structure

    with pytest.raises(ValueError, match="exon_ends"):
        isoform_structure({"transcript": ["tx1"], "exon_starts": [[1, 2]]},
                          title="t", out=tmp_path / "x.png")


def test_sashimi_renders_with_arc_thicknesses(tmp_path):
    from lib.plotting.core import sashimi

    frame = {
        "junction": ["100-300", "200-450", "150-600", "250-700"],
        "count": [12, 45, 8, 30],
    }
    out = sashimi(frame, title="sashimi",
                  out=tmp_path / "sash.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "sash.pdf").exists()


def test_sashimi_missing_column_raises(tmp_path):
    from lib.plotting.core import sashimi

    with pytest.raises(ValueError, match="count"):
        sashimi({"junction": ["1-2"]},
                title="t", out=tmp_path / "x.png")


def test_peptide_coverage_renders(tmp_path):
    from lib.plotting.core import peptide_coverage

    rng = np.random.default_rng(0)
    pos = np.arange(1, 251)
    cov = rng.integers(0, 6, size=250).astype(float)
    out = peptide_coverage({"position": pos, "coverage": cov},
                           title="protein coverage",
                           out=tmp_path / "pep.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "pep.pdf").exists()


def test_peptide_coverage_missing_column_raises(tmp_path):
    from lib.plotting.core import peptide_coverage

    with pytest.raises(ValueError, match="coverage"):
        peptide_coverage({"position": [1, 2]},
                         title="t", out=tmp_path / "x.png")


def test_ridgeline_renders_with_groups(tmp_path):
    from lib.plotting.core import ridgeline

    rng = np.random.default_rng(2)
    groups = np.repeat(["A", "B", "C"], 60)
    values = np.concatenate([
        rng.normal(0.0, 1.0, 60),
        rng.normal(1.5, 0.8, 60),
        rng.normal(-0.5, 1.2, 60),
    ])
    frame = {"group": groups, "value": values}
    out = ridgeline(frame, title="intensity ridges",
                    out=tmp_path / "ridge.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "ridge.pdf").exists()


def test_ridgeline_missing_column_raises(tmp_path):
    from lib.plotting.core import ridgeline

    with pytest.raises(ValueError, match="value"):
        ridgeline({"group": ["A", "B"]},
                  title="t", out=tmp_path / "x.png")


# ---------------------------------------------------------------------------
# Phase I (plan §S13.4-§S13.5): metagenomics + spatial transcriptomics.
# Same convention as Phase H — round-trip render + missing-column tests.
# ---------------------------------------------------------------------------


def test_taxonomic_stacked_bar_renders(tmp_path):
    from lib.plotting.core import taxonomic_stacked_bar

    rng = np.random.default_rng(3)
    samples = ["S1", "S2", "S3", "S4"] * 5
    taxa = ([f"Taxon{i}" for i in range(5)]) * 4
    abundance = rng.uniform(1, 100, size=len(samples))
    frame = {"sample": samples, "taxon": taxa, "abundance": abundance}
    out = taxonomic_stacked_bar(frame, title="relative abundance",
                                out=tmp_path / "tax.png",
                                top_n=4)
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "tax.pdf").exists()


def test_taxonomic_stacked_bar_missing_column_raises(tmp_path):
    from lib.plotting.core import taxonomic_stacked_bar

    with pytest.raises(ValueError, match="abundance"):
        taxonomic_stacked_bar({"sample": ["A"], "taxon": ["X"]},
                              title="t", out=tmp_path / "x.png")


def test_diversity_violin_renders(tmp_path):
    from lib.plotting.core import diversity_violin

    rng = np.random.default_rng(4)
    groups = np.repeat(["control", "case"], 30)
    diversity = np.concatenate([
        rng.normal(2.5, 0.4, 30),
        rng.normal(2.1, 0.5, 30),
    ])
    frame = {"group": groups, "diversity": diversity}
    out = diversity_violin(frame, title="alpha diversity",
                           out=tmp_path / "div.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "div.pdf").exists()


def test_diversity_violin_missing_column_raises(tmp_path):
    from lib.plotting.core import diversity_violin

    with pytest.raises(ValueError, match="diversity"):
        diversity_violin({"group": ["A", "B"]},
                         title="t", out=tmp_path / "x.png")


def test_tissue_overlay_renders_without_image(tmp_path):
    from lib.plotting.core import tissue_overlay

    rng = np.random.default_rng(5)
    n = 60
    frame = {
        "x": rng.uniform(0, 100, n),
        "y": rng.uniform(0, 100, n),
        "value": rng.uniform(0, 1, n),
    }
    # image=None still renders the spot scatter + colorbar.
    out = tissue_overlay(frame, image=None, title="spot overlay",
                         out=tmp_path / "tissue.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "tissue.pdf").exists()


def test_tissue_overlay_missing_column_raises(tmp_path):
    from lib.plotting.core import tissue_overlay

    with pytest.raises(ValueError, match="value"):
        tissue_overlay({"x": [1.0, 2.0], "y": [1.0, 2.0]},
                       image=None,
                       title="t", out=tmp_path / "x.png")


def test_morans_i_scatter_renders(tmp_path):
    from lib.plotting.core import morans_i_scatter

    rng = np.random.default_rng(6)
    n = 50
    genes = [f"gene{i}" for i in range(n)]
    morans_i = rng.uniform(-0.2, 0.6, n)
    pvals = np.clip(np.abs(rng.normal(0, 0.2, n)), 1e-9, 1.0)
    frame = {"gene": genes, "morans_i": morans_i, "p_value": pvals}
    out = morans_i_scatter(frame, title="spatial autocorrelation",
                           out=tmp_path / "moran.png",
                           label_top_n=3)
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "moran.pdf").exists()


def test_morans_i_scatter_missing_column_raises(tmp_path):
    from lib.plotting.core import morans_i_scatter

    with pytest.raises(ValueError, match="p_value"):
        morans_i_scatter({"gene": ["g"], "morans_i": [0.3]},
                         title="t", out=tmp_path / "x.png")


def test_neighborhood_enrichment_renders(tmp_path):
    from lib.plotting.core import neighborhood_enrichment

    domains = ["L1", "L2", "L3"]
    rows = []
    rng = np.random.default_rng(7)
    for s in domains:
        for t in domains:
            rows.append({"source": s, "target": t,
                         "score": float(rng.normal(0, 1.5))})
    frame = {
        "source": [r["source"] for r in rows],
        "target": [r["target"] for r in rows],
        "score": [r["score"] for r in rows],
    }
    out = neighborhood_enrichment(frame, title="neighborhood",
                                  out=tmp_path / "nh.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "nh.pdf").exists()


def test_neighborhood_enrichment_missing_column_raises(tmp_path):
    from lib.plotting.core import neighborhood_enrichment

    with pytest.raises(ValueError, match="score"):
        neighborhood_enrichment({"source": ["a"], "target": ["b"]},
                                title="t", out=tmp_path / "x.png")


# ── Phase J — time-series + bulk RNA-seq polish ───────────────────────────


def test_forecast_ribbon_renders_with_actual(tmp_path):
    from lib.plotting.core import forecast_ribbon

    rng = np.random.default_rng(11)
    n = 24
    t = np.arange(n)
    forecast = np.cumsum(rng.normal(0, 0.5, n))
    band = rng.uniform(0.5, 1.5, n)
    frame = {
        "time": t,
        "forecast": forecast,
        "lower": forecast - band,
        "upper": forecast + band,
        "actual": forecast + rng.normal(0, 0.3, n),
    }
    out = forecast_ribbon(frame, title="forecast",
                          out=tmp_path / "fr.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "fr.pdf").exists()


def test_forecast_ribbon_renders_with_blank_future_actuals(tmp_path):
    from lib.plotting.core import forecast_ribbon

    frame = {
        "time": [
            "2024-01", "2024-02", "2024-03",
            "2025-01", "2025-02", "2025-03",
        ],
        "series_id": ["A", "A", "A", "A", "A", "A"],
        "forecast": [101.0, 105.0, 109.0, 112.0, 115.0, 119.0],
        "lower": [96.0, 100.0, 104.0, 106.0, 109.0, 113.0],
        "upper": [106.0, 110.0, 114.0, 118.0, 121.0, 125.0],
        "actual": ["100", "106", "108", "", "", ""],
    }
    out = forecast_ribbon(
        frame,
        title="forecast",
        out=tmp_path / "fr_blank_actuals.png",
        group_col="series_id",
    )
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "fr_blank_actuals.pdf").exists()


def test_generate_forecast_ribbon_accepts_manifest_override(tmp_path):
    outputs = tmp_path / "forecasting"
    outputs.mkdir()
    (outputs / "forecast_table.tsv").write_text(
        "\n".join(
            [
                "time\tseries_id\tactual\tforecast\tlower\tupper",
                "2024-01\tA\t100\t101\t96\t106",
                "2024-02\tA\t106\t105\t100\t110",
                "2025-01\tA\t\t112\t106\t118",
                "2025-02\tA\t\t115\t109\t121",
            ]
        )
        + "\n"
    )

    mf = generate(
        "forecasting_inference",
        outputs,
        required=["forecast_ribbon"],
        manifest_override={"forecast_table": "forecast_table.tsv"},
    )

    assert "forecast_ribbon" in mf.written
    assert (outputs / "figures" / "forecast_ribbon.png").exists()
    assert (outputs / "figures" / "forecast_ribbon.pdf").exists()


def test_forecast_ribbon_missing_column_raises(tmp_path):
    from lib.plotting.core import forecast_ribbon

    with pytest.raises(ValueError, match="upper"):
        forecast_ribbon(
            {"time": [0, 1], "forecast": [0.1, 0.2], "lower": [0.0, 0.1]},
            title="t", out=tmp_path / "x.png",
        )


def test_acf_pacf_panel_renders(tmp_path):
    from lib.plotting.core import acf_pacf_panel

    rng = np.random.default_rng(12)
    # AR(1)-ish series so PACF has a clear lag-1 spike.
    n = 200
    s = np.zeros(n)
    for i in range(1, n):
        s[i] = 0.6 * s[i - 1] + rng.normal()
    out = acf_pacf_panel({"value": s}, title="acf/pacf",
                         out=tmp_path / "ap.png", max_lag=20)
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "ap.pdf").exists()


def test_acf_pacf_panel_missing_column_raises(tmp_path):
    from lib.plotting.core import acf_pacf_panel

    with pytest.raises(ValueError, match="value"):
        acf_pacf_panel({"t": [1, 2, 3]}, title="t",
                       out=tmp_path / "x.png")


def test_decomposition_panel_renders(tmp_path):
    from lib.plotting.core import decomposition_panel

    rng = np.random.default_rng(13)
    n = 96
    t = np.arange(n)
    seasonal = np.sin(2 * np.pi * t / 12) * 1.5
    trend = 0.05 * t
    frame = {
        "time": t,
        "value": trend + seasonal + rng.normal(0, 0.3, n),
    }
    out = decomposition_panel(frame, title="decomp",
                              out=tmp_path / "dec.png", period=12)
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "dec.pdf").exists()


def test_decomposition_panel_missing_column_raises(tmp_path):
    from lib.plotting.core import decomposition_panel

    with pytest.raises(ValueError, match="value"):
        decomposition_panel({"time": [0, 1, 2]},
                            title="t", out=tmp_path / "x.png")


def test_anomaly_timeline_renders_with_runs(tmp_path):
    from lib.plotting.core import anomaly_timeline

    rng = np.random.default_rng(14)
    n = 80
    t = np.arange(n)
    val = rng.normal(0, 1, n)
    flag = np.zeros(n, dtype=bool)
    # Two contiguous anomaly windows + a single-point anomaly.
    flag[10:15] = True
    flag[40:42] = True
    flag[60] = True
    frame = {"time": t, "value": val, "is_anomaly": flag}
    out = anomaly_timeline(frame, title="anomalies",
                           out=tmp_path / "an.png")
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "an.pdf").exists()


def test_anomaly_timeline_missing_column_raises(tmp_path):
    from lib.plotting.core import anomaly_timeline

    with pytest.raises(ValueError, match="is_anomaly"):
        anomaly_timeline({"time": [0, 1], "value": [0.1, 0.2]},
                         title="t", out=tmp_path / "x.png")


def test_ma_plot_renders_with_labels(tmp_path):
    from lib.plotting.core import ma_plot

    rng = np.random.default_rng(15)
    n = 80
    frame = {
        "gene": [f"g{i}" for i in range(n)],
        "base_mean": np.exp(rng.uniform(0, 6, n)),
        "log2FoldChange": rng.normal(0, 1.4, n),
        "padj": np.clip(rng.uniform(0, 0.5, n), 1e-6, 1.0),
    }
    # Force a few highly-significant entries so label paths exercise.
    frame["padj"][0] = 1e-10
    frame["log2FoldChange"][0] = 2.5
    frame["padj"][1] = 1e-9
    frame["log2FoldChange"][1] = -2.2
    out = ma_plot(frame, title="MA plot",
                  out=tmp_path / "ma.png", label_top_n=3)
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "ma.pdf").exists()


def test_ma_plot_missing_column_raises(tmp_path):
    from lib.plotting.core import ma_plot

    with pytest.raises(ValueError, match="padj"):
        ma_plot({"gene": ["g"], "base_mean": [10.0],
                 "log2FoldChange": [0.5]},
                title="t", out=tmp_path / "x.png")


# ── Phase K — cross-modality composite ────────────────────────────────────


def test_dashboard_grid_renders_mixed_panels(tmp_path):
    from lib.plotting.core import dashboard_grid

    rng = np.random.default_rng(16)
    # Volcano panel.
    n_vol = 50
    volcano_data = {
        "log_fc": rng.normal(0, 1.5, n_vol),
        "neg_log10_p": np.abs(rng.normal(0, 2, n_vol)),
    }
    # Forecast-ribbon panel.
    n_fc = 12
    forecast = np.cumsum(rng.normal(0, 0.5, n_fc))
    band = rng.uniform(0.5, 1.5, n_fc)
    forecast_data = {
        "time": np.arange(n_fc),
        "forecast": forecast,
        "lower": forecast - band,
        "upper": forecast + band,
    }
    panels = [
        {"type": "volcano", "data": volcano_data,
         "args": {"subtitle": "DE volcano"}},
        {"type": "forecast_ribbon", "data": forecast_data,
         "args": {"subtitle": "horizon forecast", "actual_col": None}},
        {"type": "unknown_type", "data": {"x": [1]},
         "args": {"subtitle": "fallback"}},
    ]
    out = dashboard_grid(panels, title="composite",
                         out=tmp_path / "dash.png", layout=(2, 2))
    assert out.exists() and out.stat().st_size > 0
    assert (tmp_path / "dash.pdf").exists()


def test_dashboard_grid_invalid_layout_raises(tmp_path):
    from lib.plotting.core import dashboard_grid

    with pytest.raises(ValueError, match="positive"):
        dashboard_grid([], title="t", out=tmp_path / "x.png",
                       layout=(0, 2))


if __name__ == "__main__":  # pragma: no cover
    pytest.main([__file__, "-v"])
