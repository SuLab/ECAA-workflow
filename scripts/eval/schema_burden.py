"""Offline schema-authoring-burden analyzer (E4).

The closed tool/atom/modality vocabulary IS the schema a contributor extends to
add a capability. This analyzer measures, from the committed tree only (no LLM,
no live gate, no network):

  - atom count (config/stage-atoms/*.yaml)
  - modality manifest count + total LOC
  - archetype count + total LOC
  - Tool::COUNT (parsed: BatchableTool variants + HighImpactTool variants)
  - BlockerKind variant count
  - the 3-artifact "files touched to add a modality" rule (CLAUDE.md)
  - median LOC + files per new modality across real git history

Counts are cross-checked against the live Rust source so the metric can't drift.
"""
from __future__ import annotations
import json
import re
import subprocess
import sys
from pathlib import Path
from statistics import median


def _yaml_files(dir_path: Path) -> list[Path]:
    """Non-underscore-prefixed *.yaml in dir (matches the atom_count gate's
    filter: name.ends_with('.yaml') && !name.starts_with('_'))."""
    if not dir_path.is_dir():
        return []
    return sorted(p for p in dir_path.glob("*.yaml") if not p.name.startswith("_"))


def count_atom_yamls(repo_root: Path) -> int:
    return len(_yaml_files(Path(repo_root) / "config" / "stage-atoms"))


def count_modality_yamls(repo_root: Path) -> int:
    return len(_yaml_files(Path(repo_root) / "config" / "modalities"))


def count_archetype_yamls(repo_root: Path) -> int:
    return len(_yaml_files(Path(repo_root) / "config" / "archetypes"))


def _total_loc(files: list[Path]) -> int:
    total = 0
    for f in files:
        try:
            total += sum(1 for _ in f.open())
        except OSError:
            continue
    return total


def _count_enum_variants(source: str, enum_name: str) -> int:
    """Count top-level variants of `enum <enum_name> { ... }` in Rust source.
    Variants are lines inside the brace block that look like `Name` / `Name {`
    / `Name(` at one indent level. Heuristic but stable for these enums."""
    m = re.search(rf"enum\s+{enum_name}\s*\{{", source)
    if not m:
        return 0
    depth = 0
    i = m.end() - 1
    body_start = None
    body_end = None
    for j in range(i, len(source)):
        if source[j] == "{":
            depth += 1
            if depth == 1:
                body_start = j + 1
        elif source[j] == "}":
            depth -= 1
            if depth == 0:
                body_end = j
                break
    if body_start is None or body_end is None:
        return 0
    body = source[body_start:body_end]
    count = 0
    for line in body.splitlines():
        s = line.strip()
        if not s or s.startswith("//") or s.startswith("#") or s.startswith("/*"):
            continue
        # A variant starts with an UpperCamel identifier.
        if re.match(r"^[A-Z][A-Za-z0-9]*\s*(\{|\(|,|$)", s):
            count += 1
    return count


def parse_tool_count_from_source(repo_root: Path) -> int:
    """Tool::COUNT = BatchableTool::COUNT + HighImpactTool::COUNT. We can't run
    the const offline, so count the variants of both bucket enums."""
    src = (Path(repo_root) / "crates" / "conversation" / "src" / "tools"
           / "mod.rs").read_text()
    return (_count_enum_variants(src, "BatchableTool")
            + _count_enum_variants(src, "HighImpactTool"))


def _find_blocker_kind_source(repo_root: Path) -> Path | None:
    """Locate the file defining `enum BlockerKind`. In this OSS surface the
    canonical definition lives in crates/ecaa-types/src/blocker.rs (NOT
    crates/core/src/blocker.rs, which only re-exports it), so search any
    blocker*.rs under crates/ for the actual `enum BlockerKind {` definition."""
    crates = Path(repo_root) / "crates"
    if not crates.is_dir():
        return None
    for candidate in sorted(crates.rglob("blocker*.rs")):
        try:
            text = candidate.read_text()
        except OSError:
            continue
        if re.search(r"enum\s+BlockerKind\s*\{", text):
            return candidate
    return None


def count_blocker_kind_variants(repo_root: Path) -> int:
    """Count BlockerKind variants from the file that DEFINES the enum."""
    src = _find_blocker_kind_source(repo_root)
    if src is None:
        return 0
    return _count_enum_variants(src.read_text(), "BlockerKind")


def files_to_add_modality() -> list[str]:
    """The 3-artifact rule from CLAUDE.md ('Adding a modality requires ...')."""
    return [
        "config/modalities/<id>.yaml",
        "config/archetypes/<id>.yaml",
        "crates/core (classifier or composer test case)",
    ]


def _modality_history(repo_root: Path) -> dict:
    """Median LOC + files-per-new-modality across the project's real git history
    of config/modalities/. Best-effort: returns {} if git is unavailable."""
    try:
        out = subprocess.check_output(
            ["git", "log", "--diff-filter=A", "--format=%H",
             "--", "config/modalities/"],
            cwd=str(repo_root), text=True)
    except (subprocess.CalledProcessError, OSError):
        return {}
    locs: list[int] = []
    for sha in (line for line in out.splitlines() if line.strip()):
        try:
            stat = subprocess.check_output(
                ["git", "show", "--numstat", "--format=", sha,
                 "--", "config/modalities/"],
                cwd=str(repo_root), text=True)
        except (subprocess.CalledProcessError, OSError):
            continue
        added = 0
        for line in stat.splitlines():
            parts = line.split("\t")
            if len(parts) >= 1 and parts[0].isdigit():
                added += int(parts[0])
        if added:
            locs.append(added)
    if not locs:
        return {}
    return {"new_modality_commits": len(locs),
            "median_added_loc_per_commit": median(locs)}


def compute_schema_burden(repo_root: Path) -> dict:
    repo_root = Path(repo_root)
    atom_files = _yaml_files(repo_root / "config" / "stage-atoms")
    modality_files = _yaml_files(repo_root / "config" / "modalities")
    archetype_files = _yaml_files(repo_root / "config" / "archetypes")
    blocker_src = _find_blocker_kind_source(repo_root)
    return {
        "atom_count": len(atom_files),
        "modality_count": len(modality_files),
        "modality_total_loc": _total_loc(modality_files),
        "archetype_count": len(archetype_files),
        "archetype_total_loc": _total_loc(archetype_files),
        "tool_count": parse_tool_count_from_source(repo_root),
        "blocker_kind_count": count_blocker_kind_variants(repo_root),
        "files_to_add_modality": files_to_add_modality(),
        "modality_git_history": _modality_history(repo_root),
        "cross_check": {
            "atom_count_source": "config/stage-atoms/*.yaml",
            "tool_count_source": "crates/conversation/src/tools/mod.rs "
                                 "(BatchableTool + HighImpactTool variants)",
            "blocker_kind_source": (str(blocker_src.relative_to(repo_root))
                                    if blocker_src is not None
                                    else "crates/ecaa-types/src/blocker.rs"),
            "note": "atom_count_baseline.rs is #[ignore]'d in OSS (no "
                    ".github/ci/expected-test-counts.json); cross-check against "
                    "`ecaa-workflow list atoms --json` length.",
        },
    }


def render_markdown(report: dict) -> str:
    lines = ["# Schema-authoring burden", "",
             "Measured offline from the committed tree. The closed tool/atom/",
             "modality vocabulary is the schema a contributor extends.", "",
             "| metric | value |", "| --- | --- |",
             f"| atom YAMLs | {report['atom_count']} |",
             f"| modality manifests | {report['modality_count']} |",
             f"| modality total LOC | {report['modality_total_loc']} |",
             f"| archetypes | {report['archetype_count']} |",
             f"| archetype total LOC | {report['archetype_total_loc']} |",
             f"| Tool::COUNT (parsed) | {report['tool_count']} |",
             f"| BlockerKind variants | {report['blocker_kind_count']} |",
             f"| files to add a modality | {len(report['files_to_add_modality'])} |",
             ""]
    hist = report.get("modality_git_history") or {}
    if hist:
        lines += [
            f"Median added LOC per new-modality commit: "
            f"{hist['median_added_loc_per_commit']} "
            f"(over {hist['new_modality_commits']} commits).", ""]
    lines.append("Files to add a modality:")
    for a in report["files_to_add_modality"]:
        lines.append(f"- `{a}`")
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    repo_root = Path(__file__).resolve().parents[2]
    report = compute_schema_burden(repo_root)
    out_dir = repo_root / "docs" / "eval-results"
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "schema-burden.json").write_text(json.dumps(report, indent=2))
    (out_dir / "schema-burden.md").write_text(render_markdown(report))
    print(f"wrote {out_dir}/schema-burden.json + schema-burden.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
