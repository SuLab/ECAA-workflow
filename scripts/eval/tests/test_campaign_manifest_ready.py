"""campaign.toml is a ready-to-run two-arm campaign: two arms pinned, paired
floor >= 10, seed mirrors the scorecard bootstrap seed, every benchmark can
reach the paired floor, and budget caps are explicitly lifted for the run.
"""
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib


def _manifest() -> dict:
    p = Path(__file__).resolve().parents[1] / "campaign.toml"
    return tomllib.loads(p.read_text())


def test_two_arms_pinned():
    m = _manifest()
    assert m["campaign"]["arms"] == ["ecaa", "claude-direct"]


def test_paired_floor_at_least_ten():
    assert _manifest()["campaign"]["min_paired_pairs"] >= 10


def test_seed_matches_bootstrap_seed():
    from scripts.eval.services.scorecard import _BOOTSTRAP_SEED
    assert _manifest()["campaign"]["seed"] == _BOOTSTRAP_SEED


def test_every_benchmark_reaches_paired_floor():
    m = _manifest()
    floor = m["campaign"]["min_paired_pairs"]
    for b in m["benchmarks"]:
        # n_pairs is capped by trials * tasks; both must reach the floor.
        cap = b.get("trials", 0) * b.get("tasks", b.get("public_tasks", 1))
        assert cap >= floor, f"{b['name']} cannot reach paired floor {floor}"


def test_run_env_lifts_budget_caps():
    m = _manifest()
    run_env = m.get("run_env", {})
    # The campaign explicitly lifts the per-task Opus budget caps so a long
    # discover_* stage is not falsely blocked as TurnBudgetExceeded.
    assert run_env.get("lift_budget_caps") is True
    assert "ECAA_AGENT_BUDGET_USD_DISCOVER" in run_env.get("budget_env", {})
