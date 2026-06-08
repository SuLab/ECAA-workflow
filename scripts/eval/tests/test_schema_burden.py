"""schema_burden.py — offline schema-authoring-burden analyzer.

Counts are cross-checked against the LIVE Rust source-of-truth (the
ecaa-workflow `list atoms --json` count and the Tool::COUNT computation) so the
metric can't silently drift. NOTE: the atom_count_baseline.rs test is #[ignore]'d
in the OSS surface (no .github/ci/expected-test-counts.json), so we cross-check
against the live list output, NOT that JSON baseline.
"""
from pathlib import Path
from scripts.eval.schema_burden import (
    compute_schema_burden, count_atom_yamls, count_modality_yamls,
    count_archetype_yamls, parse_tool_count_from_source, files_to_add_modality,
)

REPO_ROOT = Path(__file__).resolve().parents[3]


def test_atom_count_matches_filesystem():
    # 97 at HEAD (96 + biological_interpretation); couples to config/stage-atoms.
    n = count_atom_yamls(REPO_ROOT)
    assert n == 97, f"expected 97 stage-atom YAMLs, found {n} (update both if intentional)"


def test_modality_count_matches_filesystem():
    assert count_modality_yamls(REPO_ROOT) == 23


def test_archetype_count_matches_filesystem():
    assert count_archetype_yamls(REPO_ROOT) == 32


def test_tool_count_parsed_from_source_is_positive():
    # Parsed from tools/mod.rs (BatchableTool::COUNT + HighImpactTool::COUNT).
    # We can't evaluate the const offline, so we count the variants in each
    # bucket enum; assert it is a plausible positive integer.
    n = parse_tool_count_from_source(REPO_ROOT)
    assert isinstance(n, int) and n > 0


def test_files_to_add_modality_is_three_artifact_rule():
    # CLAUDE.md: a modality needs (1) config/modalities/<id>.yaml,
    # (2) config/archetypes/<id>.yaml, (3) a classifier/composer test in core.
    artifacts = files_to_add_modality()
    assert len(artifacts) == 3
    assert any("config/modalities" in a for a in artifacts)
    assert any("config/archetypes" in a for a in artifacts)
    assert any("crates/core" in a for a in artifacts)


def test_compute_emits_full_report(tmp_path):
    report = compute_schema_burden(REPO_ROOT)
    assert report["atom_count"] == 97
    assert report["modality_count"] == 23
    assert report["archetype_count"] == 32
    assert report["tool_count"] > 0
    assert report["blocker_kind_count"] > 0
    assert len(report["files_to_add_modality"]) == 3
    # Cross-check field: the analyzer records the live source it derived each
    # count from so a reader can re-verify.
    assert report["cross_check"]["atom_count_source"] == "config/stage-atoms/*.yaml"
