"""Post-hoc verifier: assert a produced run's scorecard satisfies campaign.toml.

Code-only (no live gate). The operator runs this AFTER a live campaign to prove
the committed evidence is manifest-compliant before publishing it."""
from __future__ import annotations
import json
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib


class CampaignViolation(Exception):
    """A produced scorecard does not satisfy the campaign manifest."""


def _load_manifest() -> dict:
    manifest = Path(__file__).resolve().parent / "campaign.toml"
    with manifest.open("rb") as f:
        return tomllib.load(f)


def verify_run(run_dir: Path, manifest: dict | None = None) -> dict:
    run_dir = Path(run_dir)
    manifest = manifest or _load_manifest()
    sc_path = run_dir / "scorecard.json"
    if not sc_path.exists():
        raise CampaignViolation(f"{sc_path} not found — no scorecard to verify")
    card = json.loads(sc_path.read_text())
    meta = card.get("meta", {})
    rows = card.get("rows", [])
    benchmark = card.get("benchmark", "")

    # (1) Required arms present.
    required_arms = set(manifest["campaign"]["arms"])
    present_arms = {r["arm"] for r in rows}
    missing = required_arms - present_arms
    if missing:
        raise CampaignViolation(f"missing required arm(s): {sorted(missing)}")

    # (2) Seed match (mirrors scorecard._BOOTSTRAP_SEED).
    want_seed = manifest["campaign"]["seed"]
    got_seed = meta.get("seed")
    if got_seed is not None and got_seed != want_seed:
        raise CampaignViolation(f"seed mismatch: manifest {want_seed} != scorecard {got_seed}")

    # (3) Paired-pair floor.
    floor = manifest["campaign"]["min_paired_pairs"]
    paired = meta.get("paired_delta") or {}
    n_pairs = paired.get("n_pairs", 0)
    if n_pairs < floor:
        raise CampaignViolation(
            f"underpowered: {n_pairs} paired observations < manifest floor {floor}")

    # (4) Benchmark judge/determinism matches the manifest entry (when known).
    bench_entry = next((b for b in manifest.get("benchmarks", [])
                        if b["name"] == benchmark), None)
    if bench_entry and bench_entry.get("judge") == "deterministic":
        judged = [r for r in rows if r.get("judge_id") not in ("deterministic", "", None)]
        if judged:
            raise CampaignViolation(
                f"{benchmark} declared deterministic but {len(judged)} rows carry a judge")

    return {"compliant": True, "benchmark": benchmark, "n_pairs": n_pairs,
            "arms": sorted(present_arms), "seed": got_seed or want_seed}


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print("usage: python3 -m scripts.eval.verify_campaign <run_dir>", file=sys.stderr)
        return 2
    try:
        report = verify_run(Path(argv[0]))
    except CampaignViolation as e:
        print(f"CAMPAIGN VIOLATION: {e}", file=sys.stderr)
        return 1
    print(f"campaign-compliant: {report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
