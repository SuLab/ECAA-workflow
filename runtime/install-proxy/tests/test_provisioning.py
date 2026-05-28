"""Integration tests for install-proxy shims.

Stubs the real package manager via SWFC_REAL_<TOOL> env vars; tests
that the shim correctly denies undeclared packages (exit 73) and
passes through declared ones (exit 0; logs to install-log.jsonl).
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest


SHIM_DIR = Path(__file__).resolve().parent.parent


def make_stub(stub_dir: Path, name: str) -> Path:
    """Write a stub script that exits 0 and records argv to a .args file."""
    p = stub_dir / name
    p.write_text(
        "#!/usr/bin/env python3\n"
        "import sys\n"
        "from pathlib import Path\n"
        "Path(__file__).with_suffix('.args').write_text(' '.join(sys.argv[1:]))\n"
    )
    p.chmod(0o755)
    return p


def write_policy(
    tmp_path: Path,
    provisioning: str,
    declared: dict | None = None,
    allowed_registries: list | None = None,
) -> Path:
    p = tmp_path / "provisioning.json"
    p.write_text(json.dumps({
        "provisioning": provisioning,
        "atom_id": "test_atom",
        "declared_packages": declared or {},
        "allowed_registries": allowed_registries or [],
    }))
    return p


def run_shim(
    name: str,
    *args: str,
    env_overrides: dict[str, str],
) -> subprocess.CompletedProcess:
    shim = SHIM_DIR / f"{name}.py"
    env = os.environ.copy()
    env.update(env_overrides)
    return subprocess.run(
        [sys.executable, str(shim), *args],
        env=env,
        capture_output=True,
        text=True,
    )


# -- apt --

def test_apt_denies_undeclared(tmp_path):
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    make_stub(stub_dir, "apt")
    policy = write_policy(tmp_path, "declared_only", declared={"apt": ["samtools"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "apt", "install", "-y", "wget",
        env_overrides={
            "SWFC_REAL_APT": str(stub_dir / "apt"),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 73
    assert "provisioning_denied" in result.stdout


def test_apt_accepts_declared(tmp_path):
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    stub = make_stub(stub_dir, "apt")
    policy = write_policy(tmp_path, "declared_only", declared={"apt": ["samtools"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "apt", "install", "-y", "samtools",
        env_overrides={
            "SWFC_REAL_APT": str(stub),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 0, result.stderr
    assert log.exists()
    entries = [json.loads(l) for l in log.read_text().splitlines()]
    assert any(e["package"] == "samtools" and e["registry"] == "apt" for e in entries)


# -- pip --

def test_pip_denies_undeclared(tmp_path):
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    make_stub(stub_dir, "pip")
    policy = write_policy(tmp_path, "declared_only", declared={"pip": ["numpy"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "pip", "install", "requests",
        env_overrides={
            "SWFC_REAL_PIP": str(stub_dir / "pip"),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 73
    assert "provisioning_denied" in result.stdout


def test_pip_accepts_declared(tmp_path):
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    stub = make_stub(stub_dir, "pip")
    policy = write_policy(tmp_path, "declared_only", declared={"pip": ["numpy"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "pip", "install", "numpy",
        env_overrides={
            "SWFC_REAL_PIP": str(stub),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 0, result.stderr
    entries = [json.loads(l) for l in log.read_text().splitlines()]
    assert any(e["package"] == "numpy" and e["registry"] == "pip" for e in entries)


# -- conda --

def test_conda_denies_undeclared_bioconda(tmp_path):
    """Conda channel parsing: -c bioconda samtools → registry "bioconda"."""
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    make_stub(stub_dir, "conda")
    # Declare samtools in bioconda, but request wget — denied.
    policy = write_policy(tmp_path, "declared_only", declared={"bioconda": ["samtools"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "conda", "install", "-c", "bioconda", "wget",
        env_overrides={
            "SWFC_REAL_CONDA": str(stub_dir / "conda"),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 73
    assert "provisioning_denied" in result.stdout


def test_conda_accepts_declared_bioconda(tmp_path):
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    stub = make_stub(stub_dir, "conda")
    policy = write_policy(tmp_path, "declared_only", declared={"bioconda": ["samtools"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "conda", "install", "-c", "bioconda", "samtools=1.17",
        env_overrides={
            "SWFC_REAL_CONDA": str(stub),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 0, result.stderr
    entries = [json.loads(l) for l in log.read_text().splitlines()]
    # bare name (samtools) is what gets logged after split("=")[0]
    assert any(e["package"] == "samtools" and e["registry"] == "bioconda" for e in entries)


# -- npm --

def test_npm_denies_undeclared(tmp_path):
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    make_stub(stub_dir, "npm")
    policy = write_policy(tmp_path, "declared_only", declared={"npm": ["lodash"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "npm", "install", "express",
        env_overrides={
            "SWFC_REAL_NPM": str(stub_dir / "npm"),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 73
    assert "provisioning_denied" in result.stdout


def test_npm_accepts_declared(tmp_path):
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    stub = make_stub(stub_dir, "npm")
    policy = write_policy(tmp_path, "declared_only", declared={"npm": ["lodash"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "npm", "install", "lodash",
        env_overrides={
            "SWFC_REAL_NPM": str(stub),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 0, result.stderr
    entries = [json.loads(l) for l in log.read_text().splitlines()]
    assert any(e["package"] == "lodash" and e["registry"] == "npm" for e in entries)


# -- rscript --

def test_rscript_denies_undeclared_cran(tmp_path):
    """Rscript -e 'install.packages("data.table")' against a policy that
    only declares dplyr → deny."""
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    make_stub(stub_dir, "Rscript")
    policy = write_policy(tmp_path, "declared_only", declared={"cran": ["dplyr"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "rscript", "-e", 'install.packages("data.table")',
        env_overrides={
            "SWFC_REAL_RSCRIPT": str(stub_dir / "Rscript"),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 73
    assert "provisioning_denied" in result.stdout


def test_rscript_accepts_declared_bioconductor(tmp_path):
    """BiocManager::install("DESeq2") → registry "bioconductor"."""
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    stub = make_stub(stub_dir, "Rscript")
    policy = write_policy(tmp_path, "declared_only", declared={"bioconductor": ["DESeq2"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "rscript", "-e", 'BiocManager::install("DESeq2")',
        env_overrides={
            "SWFC_REAL_RSCRIPT": str(stub),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 0, result.stderr
    entries = [json.loads(l) for l in log.read_text().splitlines()]
    assert any(
        e["package"] == "DESeq2" and e["registry"] == "bioconductor"
        for e in entries
    )


# -- gem --

def test_gem_denies_undeclared(tmp_path):
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    make_stub(stub_dir, "gem")
    policy = write_policy(tmp_path, "declared_only", declared={"rubygems": ["rake"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "gem", "install", "bundler",
        env_overrides={
            "SWFC_REAL_GEM": str(stub_dir / "gem"),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 73
    assert "provisioning_denied" in result.stdout


def test_gem_accepts_declared(tmp_path):
    stub_dir = tmp_path / "real"; stub_dir.mkdir()
    stub = make_stub(stub_dir, "gem")
    policy = write_policy(tmp_path, "declared_only", declared={"rubygems": ["rake"]})
    log = tmp_path / "install-log.jsonl"

    result = run_shim(
        "gem", "install", "rake",
        env_overrides={
            "SWFC_REAL_GEM": str(stub),
            "SWFC_INSTALL_LOG": str(log),
            "SWFC_PROVISIONING_POLICY": str(policy),
        },
    )
    assert result.returncode == 0, result.stderr
    entries = [json.loads(l) for l in log.read_text().splitlines()]
    assert any(e["package"] == "rake" and e["registry"] == "rubygems" for e in entries)
