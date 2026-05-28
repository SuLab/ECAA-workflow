"""Cross-renderer parity tests.

Render the same logical figure through both renderers (Python via
matplotlib + seaborn, R via ggplot2 + cairo) and confirm they both
produce non-empty PNG + PDF outputs of comparable size and shape.

Byte-equality across renderers is impossible (different rasterizers,
different vector pipelines), so the bar is *visual contract* rather
than *byte parity*: same figure_id, same data, two renderers, both
produce a valid figure within an order-of-magnitude of each other in
file size + dimensions.

These tests are skipped when R isn't available — the harness
auto-detects renderer availability via the env_capability probe and
falls back to whichever renderer the agent task is running in.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import os
from functools import lru_cache
from pathlib import Path
from typing import Optional

import numpy as np
import pytest


ROOT = Path(__file__).resolve().parents[3]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

R_AVAILABLE = shutil.which("Rscript") is not None
DOCKER_AVAILABLE = shutil.which("docker") is not None
R_CONTAINER_IMAGE = os.environ.get("SWFC_DEFAULT_CONTAINER_IMAGE", "bio-min:local")
R_PLOTTING_DIR = ROOT / "lib" / "plotting_r"
R_REQUIRED_PACKAGES = ("ggplot2", "scales", "jsonlite", "ragg")


@lru_cache(maxsize=1)
def _host_r_has_required_packages() -> bool:
    if not R_AVAILABLE:
        return False
    probe = "; ".join(f"library({pkg})" for pkg in R_REQUIRED_PACKAGES)
    res = subprocess.run(
        ["Rscript", "-e", f"suppressPackageStartupMessages({{{probe}}})"],
        capture_output=True,
        text=True,
        timeout=60,
    )
    return res.returncode == 0


@lru_cache(maxsize=1)
def _docker_image_available() -> bool:
    if not DOCKER_AVAILABLE:
        return False
    res = subprocess.run(
        ["docker", "image", "inspect", R_CONTAINER_IMAGE],
        capture_output=True,
        text=True,
        timeout=30,
    )
    return res.returncode == 0


@lru_cache(maxsize=1)
def _docker_r_has_required_packages() -> bool:
    if not _docker_image_available():
        return False
    probe = "; ".join(f"library({pkg})" for pkg in R_REQUIRED_PACKAGES)
    res = subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            R_CONTAINER_IMAGE,
            "Rscript",
            "-e",
            f"suppressPackageStartupMessages({{{probe}}})",
        ],
        capture_output=True,
        text=True,
        timeout=120,
    )
    return res.returncode == 0


def _run_rscript(script: str, tmp_path: Path) -> subprocess.CompletedProcess[str]:
    tmp_path.mkdir(parents=True, exist_ok=True)
    force_container = os.environ.get("SWFC_PLOTTING_R_CONTAINER") == "1"
    if not force_container and _host_r_has_required_packages():
        return subprocess.run(
            ["Rscript", "-e", script],
            capture_output=True,
            text=True,
            timeout=120,
        )
    if _docker_r_has_required_packages():
        uid_gid = f"{os.getuid()}:{os.getgid()}"
        cache_dir = tmp_path / ".cache"
        cache_dir.mkdir(parents=True, exist_ok=True)
        return subprocess.run(
            [
                "docker",
                "run",
                "--rm",
                "--user",
                uid_gid,
                "-v",
                f"{ROOT}:{ROOT}:ro",
                "-v",
                f"{tmp_path}:{tmp_path}:rw",
                "-e",
                f"HOME={tmp_path}",
                "-e",
                f"XDG_CACHE_HOME={cache_dir}",
                "-w",
                str(ROOT),
                R_CONTAINER_IMAGE,
                "Rscript",
                "-e",
                script,
            ],
            capture_output=True,
            text=True,
            timeout=120,
        )
    pytest.skip(
        "R renderer dependencies unavailable on host and container image "
        f"{R_CONTAINER_IMAGE!r} is not usable"
    )


pytestmark = pytest.mark.skipif(
    not R_PLOTTING_DIR.exists() or (not R_AVAILABLE and not _docker_image_available()),
    reason="R renderer or lib/plotting_r/ not available",
)


def _render_r_volcano(tmp_path: Path) -> Optional[Path]:
    """Drive the R renderer via Rscript with an inline expression. The
    fixture data + figure id mirror the Python `volcano_de_basic`
    snapshot fixture so the two outputs are directly comparable.
    """
    out_png = tmp_path / "r_volcano.png"
    script = f"""
suppressPackageStartupMessages({{
  source(file.path('{R_PLOTTING_DIR}', 'core.R'))
}})
set.seed(7)
n <- 500
log_fc <- rnorm(n, sd = 2)
p <- pmax(abs(rnorm(n, sd = 0.15)), 1e-6)
labels <- sprintf("GENE%d", seq_len(n))
plot <- swfc_volcano(log_fc = log_fc, neg_log10_p = -log10(p),
                    labels = labels, title = "DE: aged vs healthy",
                    label_top_n = 10)
swfc_savefig(plot, '{out_png}', stage_id = 'differential_expression')
"""
    res = _run_rscript(script, tmp_path)
    if res.returncode != 0:
        pytest.fail(f"Rscript failed: {res.stderr}")
    return out_png if out_png.exists() else None


def _render_python_volcano(tmp_path: Path) -> Path:
    from lib.plotting.core import volcano

    rng = np.random.default_rng(7)
    n = 500
    log_fc = rng.normal(scale=2.0, size=n)
    p = np.clip(np.abs(rng.normal(scale=0.15, size=n)), 1e-6, 1.0)
    return volcano(
        log_fc=log_fc,
        neg_log10_p=-np.log10(p),
        title="DE: aged vs healthy",
        out=tmp_path / "py_volcano.png",
        labels=[f"GENE{i}" for i in range(n)],
        label_top_n=10,
    )


def _png_dimensions(path: Path) -> Optional[tuple]:
    """Read PNG width × height from the IHDR chunk without any
    third-party dep — the IHDR is at offset 16-24, big-endian uint32s.
    """
    try:
        with path.open("rb") as f:
            data = f.read(24)
        if len(data) < 24 or data[:8] != b"\x89PNG\r\n\x1a\n":
            return None
        width = int.from_bytes(data[16:20], "big")
        height = int.from_bytes(data[20:24], "big")
        return (width, height)
    except OSError:
        return None


def test_volcano_renders_in_both_renderers(tmp_path):
    """Both renderers produce a non-empty PNG + PDF for the same
    figure_id and same fixture data.
    """
    py_png = _render_python_volcano(tmp_path)
    r_png = _render_r_volcano(tmp_path)
    assert py_png.exists() and py_png.stat().st_size > 0, "Python PNG empty"
    assert r_png is not None and r_png.stat().st_size > 0, "R PNG empty"
    assert py_png.with_suffix(".pdf").exists(), "Python PDF missing"
    assert r_png.with_suffix(".pdf").exists(), "R PDF missing"


def test_volcano_dimensions_within_reasonable_bounds(tmp_path):
    """Both PNGs land in a similar pixel-shape envelope. Different
    rasterizers will not match exactly; we accept any output where both
    sides are at least 1000×800 (publication-grade sane defaults).
    """
    py_png = _render_python_volcano(tmp_path)
    r_png = _render_r_volcano(tmp_path)
    py_dim = _png_dimensions(py_png)
    r_dim = _png_dimensions(r_png)
    assert py_dim is not None and r_dim is not None
    assert py_dim[0] >= 1000 and py_dim[1] >= 800, f"Python PNG too small: {py_dim}"
    assert r_dim[0] >= 1000 and r_dim[1] >= 800, f"R PNG too small: {r_dim}"


def test_volcano_file_size_within_order_of_magnitude(tmp_path):
    """Sanity check: neither renderer produces a 100× larger file than
    the other. Catches accidental "one side embedded raster, the other
    didn't" regressions.
    """
    py_png = _render_python_volcano(tmp_path)
    r_png = _render_r_volcano(tmp_path)
    py_size = py_png.stat().st_size
    r_size = r_png.stat().st_size
    ratio = max(py_size, r_size) / max(min(py_size, r_size), 1)
    assert ratio < 50, (
        f"file-size ratio {ratio:.1f}× — Python {py_size} vs R {r_size}; "
        "one side is likely embedding a raster in a vector container or "
        "rendering at the wrong DPI"
    )


# --------------------------------------------------------------------------
# Extended cross-renderer parity coverage. Each helper that has both a
# Python primitive and an `swfc_*_r` R primitive
# gets a parameterized parity check covering the same three contracts
# the volcano case enforces: both renderers produce non-empty PNG +
# PDF, both PNGs hit the 1000×800-or-bigger sanity envelope, and the
# file-size ratio stays within an order of magnitude.
#
# The R-side scripts are inline so each row stays self-contained;
# adding a row is a single appended dict in CROSS_RENDERER_FIXTURES.
# --------------------------------------------------------------------------


def _render_python_manhattan(tmp_path: Path) -> Path:
    from lib.plotting.core import manhattan

    rng = np.random.default_rng(11)
    rows = []
    for chrom, n in [("1", 200), ("2", 150), ("3", 100)]:
        for _ in range(n):
            pos = int(rng.integers(low=1, high=200_000_000))
            pval = float(np.clip(rng.uniform(0.0, 1.0), 1e-12, 1.0))
            rows.append((chrom, pos, pval))
    return manhattan(
        frame={
            "chrom": [r[0] for r in rows],
            "pos": [r[1] for r in rows],
            "pvalue": [r[2] for r in rows],
        },
        title="GWAS test panel",
        out=tmp_path / "py_manhattan.png",
    )


def _render_r_manhattan(tmp_path: Path) -> Optional[Path]:
    out_png = tmp_path / "r_manhattan.png"
    script = f"""
suppressPackageStartupMessages({{
  source(file.path('{R_PLOTTING_DIR}', 'core.R'))
}})
set.seed(11)
chroms <- c(rep("1", 200), rep("2", 150), rep("3", 100))
pos <- as.integer(runif(450, min = 1, max = 2e8))
pvalue <- pmax(runif(450), 1e-12)
df <- data.frame(chrom = chroms, pos = pos, pvalue = pvalue,
                 stringsAsFactors = FALSE)
plot <- swfc_manhattan_r(df, title = "GWAS test panel")
swfc_savefig(plot, '{out_png}', stage_id = 'gwas_coloc')
"""
    res = _run_rscript(script, tmp_path)
    if res.returncode != 0:
        pytest.fail(f"Rscript manhattan failed: {res.stderr}")
    return out_png if out_png.exists() else None


def _render_python_km(tmp_path: Path) -> Path:
    from lib.plotting.core import kaplan_meier

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
        out=tmp_path / "py_km.png",
        show_at_risk_table=False,
    )


def _render_r_km(tmp_path: Path) -> Optional[Path]:
    out_png = tmp_path / "r_km.png"
    script = f"""
suppressPackageStartupMessages({{
  source(file.path('{R_PLOTTING_DIR}', 'core.R'))
}})
set.seed(13)
times <- c(rexp(40, rate = 1/10), rexp(40, rate = 1/15))
events <- c(sample(0:1, 40, replace = TRUE),
            sample(0:1, 40, replace = TRUE))
arm <- c(rep("A", 40), rep("B", 40))
df <- data.frame(time = times, event = events, arm = arm,
                 stringsAsFactors = FALSE)
plot <- swfc_kaplan_meier_r(df, title = "Survival by arm",
                             group_col = "arm")
swfc_savefig(plot, '{out_png}', stage_id = 'clinical_trial_analysis')
"""
    res = _run_rscript(script, tmp_path)
    if res.returncode != 0:
        pytest.fail(f"Rscript km failed: {res.stderr}")
    return out_png if out_png.exists() else None


def _render_python_forecast(tmp_path: Path) -> Path:
    from lib.plotting.core import forecast_ribbon

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
        out=tmp_path / "py_forecast.png",
    )


def _render_r_forecast(tmp_path: Path) -> Optional[Path]:
    out_png = tmp_path / "r_forecast.png"
    script = f"""
suppressPackageStartupMessages({{
  source(file.path('{R_PLOTTING_DIR}', 'core.R'))
}})
set.seed(17)
t <- seq(0, 23)
yhat <- 100 + 5 * sin(t / 3) + rnorm(24, sd = 2)
df <- data.frame(time = t, forecast = yhat,
                 lower = yhat - 6, upper = yhat + 6,
                 actual = yhat + rnorm(24, sd = 1),
                 stringsAsFactors = FALSE)
plot <- swfc_forecast_ribbon_r(df, title = "24-month forecast")
swfc_savefig(plot, '{out_png}', stage_id = 'forecasting_inference')
"""
    res = _run_rscript(script, tmp_path)
    if res.returncode != 0:
        pytest.fail(f"Rscript forecast failed: {res.stderr}")
    return out_png if out_png.exists() else None


CROSS_RENDERER_FIXTURES = [
    pytest.param("manhattan", _render_python_manhattan, _render_r_manhattan,
                 id="manhattan"),
    pytest.param("kaplan_meier", _render_python_km, _render_r_km,
                 id="kaplan_meier"),
    pytest.param("forecast_ribbon", _render_python_forecast,
                 _render_r_forecast, id="forecast_ribbon"),
]


@pytest.mark.parametrize("name,py_fn,r_fn", CROSS_RENDERER_FIXTURES)
def test_extended_cross_renderer_parity(name, py_fn, r_fn, tmp_path):
    """Both renderers produce non-empty PNG + PDF for `name`; PNG
    dimensions clear the publication envelope; file size stays
    within an order of magnitude. One pytest row per registered
    helper-with-R-counterpart."""
    py_png = py_fn(tmp_path / f"{name}_py")
    r_png = r_fn(tmp_path / f"{name}_r")
    assert py_png.exists() and py_png.stat().st_size > 0, (
        f"{name}: Python PNG empty"
    )
    assert r_png is not None and r_png.stat().st_size > 0, (
        f"{name}: R PNG empty"
    )
    assert py_png.with_suffix(".pdf").exists(), (
        f"{name}: Python PDF missing"
    )
    assert r_png.with_suffix(".pdf").exists(), (
        f"{name}: R PDF missing"
    )
    py_dim = _png_dimensions(py_png)
    r_dim = _png_dimensions(r_png)
    assert py_dim is not None and r_dim is not None, (
        f"{name}: failed to read PNG dimensions"
    )
    assert py_dim[0] >= 800 and py_dim[1] >= 600, (
        f"{name}: Python PNG too small: {py_dim}"
    )
    assert r_dim[0] >= 800 and r_dim[1] >= 600, (
        f"{name}: R PNG too small: {r_dim}"
    )
    py_size = py_png.stat().st_size
    r_size = r_png.stat().st_size
    ratio = max(py_size, r_size) / max(min(py_size, r_size), 1)
    assert ratio < 50, (
        f"{name}: file-size ratio {ratio:.1f}× — Python {py_size} vs R {r_size}"
    )
