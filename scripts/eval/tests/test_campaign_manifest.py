"""The committed campaign manifest is well-formed and self-consistent."""
from pathlib import Path

try:
    import tomllib  # py3.11+
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib

REPO_ROOT = Path(__file__).resolve().parents[3]
MANIFEST = REPO_ROOT / "scripts" / "eval" / "campaign.toml"


def _load():
    with MANIFEST.open("rb") as f:
        return tomllib.load(f)


def test_manifest_exists_and_parses():
    assert MANIFEST.exists()
    _load()


def test_manifest_declares_required_fields():
    m = _load()
    assert m["campaign"]["seed"] == 1729  # mirrors scorecard._BOOTSTRAP_SEED
    assert m["campaign"]["min_paired_pairs"] >= 10  # mirrors _MIN_POWER_PAIRS
    bench = {b["name"] for b in m["benchmarks"]}
    assert bench == {"nekrutenko", "biomnibench"}
    arms = set(m["campaign"]["arms"])
    assert {"ecaa", "claude-direct"} <= arms


def test_nekrutenko_is_deterministic_no_judge():
    m = _load()
    nek = next(b for b in m["benchmarks"] if b["name"] == "nekrutenko")
    assert nek["judge"] == "deterministic"
    assert nek["error_matrix"] is True
    bbench = next(b for b in m["benchmarks"] if b["name"] == "biomnibench")
    assert bbench["judge"] == "gemini-3.1-pro"
    assert bbench["public_tasks"] == 50
