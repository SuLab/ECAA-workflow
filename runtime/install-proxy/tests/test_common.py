"""Tests for shared shim helpers."""
import json
import sys
import tempfile
from pathlib import Path

import pytest

# Path manipulation so the test can find _common.py (one dir up).
sys.path.insert(0, str(Path(__file__).parent.parent))
import _common  # noqa: E402


def test_load_policy_reads_json(tmp_path):
    policy_path = tmp_path / "provisioning.json"
    policy_path.write_text(json.dumps({
        "provisioning": "declared_only",
        "atom_id": "align_reads",
        "declared_packages": {"apt": ["samtools", "bwa"]},
    }))
    p = _common.load_policy(str(policy_path))
    assert p.provisioning == "declared_only"
    assert p.atom_id == "align_reads"
    assert "samtools" in p.declared_packages["apt"]


def test_load_policy_honors_env_override(tmp_path, monkeypatch):
    policy_path = tmp_path / "elsewhere.json"
    policy_path.write_text(json.dumps({
        "provisioning": "sealed",
        "atom_id": "x",
    }))
    monkeypatch.setenv("ECAA_PROVISIONING_POLICY", str(policy_path))
    p = _common.load_policy()
    assert p.provisioning == "sealed"


def test_check_allowed_sealed_denies_everything():
    p = _common.Policy(
        provisioning="sealed",
        atom_id="x",
        declared_packages={},
        allowed_registries=[],
    )
    decision = _common.check_allowed(p, "apt", "samtools")
    assert decision.allowed is False
    assert "sealed" in decision.reason.lower()


def test_check_allowed_declared_only_accepts_declared():
    p = _common.Policy(
        provisioning="declared_only",
        atom_id="x",
        declared_packages={"apt": ["samtools"]},
        allowed_registries=[],
    )
    assert _common.check_allowed(p, "apt", "samtools").allowed is True
    assert _common.check_allowed(p, "apt", "bwa").allowed is False


def test_check_allowed_allowlisted_accepts_anything_in_allowed_registry():
    p = _common.Policy(
        provisioning="allowlisted",
        atom_id="x",
        declared_packages={},
        allowed_registries=["pip", "apt"],
    )
    assert _common.check_allowed(p, "pip", "anything").allowed is True
    assert _common.check_allowed(p, "gem", "anything").allowed is False


def test_check_allowed_unknown_policy_denies():
    p = _common.Policy(
        provisioning="bogus_value",
        atom_id="x",
        declared_packages={},
        allowed_registries=[],
    )
    decision = _common.check_allowed(p, "apt", "samtools")
    assert decision.allowed is False


def test_log_install_appends_to_jsonl(tmp_path):
    log_path = tmp_path / "install-log.jsonl"
    _common.log_install(str(log_path), atom_id="x", package="samtools", registry="apt")
    _common.log_install(str(log_path), atom_id="x", package="bwa", registry="apt")
    entries = [json.loads(line) for line in log_path.read_text().splitlines()]
    assert len(entries) == 2
    assert entries[0]["package"] == "samtools"
    assert entries[1]["package"] == "bwa"
    assert entries[0]["source"] == "agent_runtime"
    assert "timestamp" in entries[0]


def test_bypass_enabled(monkeypatch):
    monkeypatch.delenv("ECAA_PROVISIONING_DISABLE", raising=False)
    assert _common.bypass_enabled() is False
    monkeypatch.setenv("ECAA_PROVISIONING_DISABLE", "1")
    assert _common.bypass_enabled() is True
    monkeypatch.setenv("ECAA_PROVISIONING_DISABLE", "0")
    assert _common.bypass_enabled() is False
