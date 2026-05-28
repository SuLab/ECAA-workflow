#!/usr/bin/env python3
"""Helpers for portable repo-root, executable, and markdown-doc hygiene tasks."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re


REPO_ROOT = Path(__file__).resolve().parent.parent
DOCS_ARCHIVE = REPO_ROOT / "docs" / "archive"
ARCHIVE_2026_03 = DOCS_ARCHIVE / "2026-03"
REPO_ABSOLUTE_PREFIX = f"{REPO_ROOT.as_posix()}/"
MARKDOWN_LINK_RE = re.compile(r"(?P<bang>!?)\[(?P<label>[^\]]+)\]\((?P<target>[^)]+)\)")
PLAIN_REPO_PATH_RE = re.compile(rf"{re.escape(REPO_ABSOLUTE_PREFIX)}[^\s)`>]+")
NON_PORTABLE_PATH_RE = re.compile(r"/[h]ome/|/[U]sers/|f[i]le://")
GOVERNED_DOC_NAME_RE = re.compile(
    r"(plan|roadmap|backlog|checklist|analysis|audit|spec|map)", re.IGNORECASE
)
SEMANTIC_PLAN_DOC_NAME_RE = re.compile(r"(plan|roadmap|backlog)", re.IGNORECASE)
STATUS_PREFIXES = ("Status:", "**Status**:", "- **Status**:")
DATE_PREFIXES = ("Date:", "**Date**:", "- **Date**:")
CURRENT_PLAN_STATUS_PREFIXES = ("Current plan status:",)
HISTORICAL_MARKER_PREFIXES = (
    "Historical baseline:",
    "Historical snapshot:",
    "Continuation note:",
    "Superseded by:",
)
ACTIVE_STATUS_TERMS = (
    "active remediation",
    "in progress",
    "approved plan",
    "proposed",
    "active follow-on",
)
HISTORICAL_STATUS_TERMS = (
    "historical",
    "superseded",
    "implemented historical",
    "authoritative historical",
)
CURRENT_PLAN_ROUTING_PHRASES = (
    "current follow-on plan",
    "current repository-level follow-on actions",
)
NO_OPEN_PHASES_LINE_RE = re.compile(r"^\s*(?:[-*]\s*)?no open phases remain\b", re.IGNORECASE)
HISTORICAL_CLOSE_OUT_STATUS_RE = re.compile(
    r"^\s*Status:.*historical close-out (?:definition|record)\b",
    re.IGNORECASE,
)
LANDED_EXIT_CRITERIA_STATUS_RE = re.compile(
    r"^\s*Current status.*all .* exit criteria are landed\b",
    re.IGNORECASE,
)
BOUNDED_CLOSE_OUT_CONTEXT_MARKERS = (
    "bounded historical statement",
    "not the current file-wide plan status",
    "use the tracked status appendix",
)
RETAINED_HISTORICAL_DOCS = {
    "docs/2026-03/autonomous-package-remediation-plan-2026-03-21.md",
    "docs/2026-03/chat-test-remediation-plan.md",
    "docs/2026-03/comprehensive-remediation-plan-2026-03-22.md",
    "docs/2026-03/execution-document-remediation-plan-2026-03-21.md",
    "docs/2026-03/ivd-live-remediation-plan-2026-03-17.md",
    "docs/2026-03/snapshot-extraction-remediation-plan-2026-03-22.md",
}
ARCHIVED_SNAPSHOT_DOCS = {
    "docs/archive/2026-03/architecture-risks-watchpoints-2026-03-16.md",
}


def repo_root() -> Path:
    return REPO_ROOT


def repo_file(*parts: str) -> Path:
    return REPO_ROOT.joinpath(*parts)


def default_markdown_files() -> list[Path]:
    files = [REPO_ROOT / "README.md"]
    files.extend(sorted((REPO_ROOT / "docs").rglob("*.md")))
    return [path for path in files if path.exists()]


def default_archive_markdown_files() -> list[Path]:
    if not ARCHIVE_2026_03.exists():
        return []
    return sorted(ARCHIVE_2026_03.glob("*.md"))


def parse_paths(values: list[str] | None, default_factory) -> list[Path]:
    raw_paths = values or []
    if not raw_paths:
        return default_factory()

    paths: list[Path] = []
    for raw in raw_paths:
        path = (REPO_ROOT / raw).resolve() if not os.path.isabs(raw) else Path(raw).resolve()
        if path.is_dir():
            paths.extend(sorted(path.rglob("*.md")))
        elif path.exists():
            paths.append(path)
    return paths


def split_anchor(target: str) -> tuple[str, str]:
    if "#" not in target:
        return target, ""
    base, anchor = target.split("#", 1)
    return base, f"#{anchor}"


def looks_external(target: str) -> bool:
    return bool(re.match(r"^[A-Za-z][A-Za-z0-9+.-]*:", target))


def repo_relative_from_absolute(target: str) -> Path | None:
    if not target.startswith(REPO_ABSOLUTE_PREFIX):
        return None
    return Path(target.removeprefix(REPO_ABSOLUTE_PREFIX))


def archive_alias_candidates(repo_relative: Path) -> list[Path]:
    candidates: list[Path] = []
    if repo_relative.parts and repo_relative.parts[0] == "docs":
        alias = ARCHIVE_2026_03 / repo_relative.name
        if alias.exists():
            candidates.append(alias)
    return candidates


def former_location_candidates(source: Path, target_path: str) -> list[Path]:
    if not source.is_relative_to(ARCHIVE_2026_03):
        return []

    candidates: list[Path] = []
    former_dirs = (
        REPO_ROOT / "docs",
        REPO_ROOT / "docs" / "2026-03",
    )
    for former_dir in former_dirs:
        candidate = (former_dir / target_path).resolve()
        if candidate.exists():
            candidates.append(candidate)
    basename = Path(target_path).name
    if basename:
        alias = ARCHIVE_2026_03 / basename
        if alias.exists():
            candidates.append(alias)
    return dedupe_paths(candidates)


def dedupe_paths(paths: list[Path]) -> list[Path]:
    seen: set[str] = set()
    deduped: list[Path] = []
    for path in paths:
        key = path.as_posix()
        if key not in seen:
            seen.add(key)
            deduped.append(path)
    return deduped


def resolve_local_target(source: Path, raw_target: str) -> Path | None:
    target, _anchor = split_anchor(raw_target)
    if not target or target.startswith("#") or looks_external(target):
        return None

    repo_relative = repo_relative_from_absolute(target)
    candidates: list[Path] = []
    if repo_relative is not None:
        direct = REPO_ROOT / repo_relative
        if direct.exists():
            candidates.append(direct)
        candidates.extend(archive_alias_candidates(repo_relative))
    else:
        direct = (source.parent / target).resolve()
        if direct.exists():
            candidates.append(direct)
        candidates.extend(former_location_candidates(source, target))

    candidates = dedupe_paths(candidates)
    return candidates[0] if candidates else None


def relativize(source: Path, target: Path, anchor: str = "") -> str:
    relative = os.path.relpath(target, source.parent)
    return Path(relative).as_posix() + anchor


def render_non_link_target(label: str, raw_target: str) -> str:
    repo_relative = repo_relative_from_absolute(raw_target)
    text = repo_relative.as_posix() if repo_relative is not None else label
    text = text.strip("`")
    return f"`{text}`"


def rewrite_markdown_links(source: Path, text: str) -> tuple[str, int]:
    replacements = 0

    def replace(match: re.Match[str]) -> str:
        nonlocal replacements
        if match.group("bang") == "!":
            return match.group(0)

        label = match.group("label")
        raw_target = match.group("target").strip()
        target, anchor = split_anchor(raw_target)

        if target.startswith("#") or looks_external(target):
            return match.group(0)

        resolved = resolve_local_target(source, raw_target)
        if resolved is not None:
            new_target = relativize(source, resolved, anchor)
            if new_target != raw_target:
                replacements += 1
                return f"[{label}]({new_target})"
            return match.group(0)

        if repo_relative_from_absolute(target) is not None:
            replacements += 1
            return render_non_link_target(label, target)

        return match.group(0)

    return MARKDOWN_LINK_RE.sub(replace, text), replacements


def rewrite_plain_repo_paths(text: str) -> tuple[str, int]:
    replacements = 0

    def replace(match: re.Match[str]) -> str:
        nonlocal replacements
        replacements += 1
        absolute = match.group(0)
        repo_relative = absolute.removeprefix(REPO_ABSOLUTE_PREFIX)
        return repo_relative

    return PLAIN_REPO_PATH_RE.sub(replace, text), replacements


def repo_relative_path(path: Path) -> str:
    return path.relative_to(REPO_ROOT).as_posix()


def first_lines(text: str, limit: int) -> list[str]:
    return text.splitlines()[:limit]


def has_prefixed_header(lines: list[str], prefixes: tuple[str, ...]) -> bool:
    for line in lines:
        stripped = line.strip()
        if any(stripped.startswith(prefix) for prefix in prefixes):
            return True
    return False


def find_prefixed_header(lines: list[str], prefixes: tuple[str, ...]) -> str | None:
    for line in lines:
        stripped = line.strip()
        if any(stripped.startswith(prefix) for prefix in prefixes):
            return stripped
    return None


def doc_requires_governance_headers(path: Path) -> bool:
    if not path.is_relative_to(REPO_ROOT / "docs"):
        return False
    if path.is_relative_to(DOCS_ARCHIVE):
        return False
    return bool(GOVERNED_DOC_NAME_RE.search(path.name))


def doc_requires_semantic_plan_status_audit(path: Path) -> bool:
    if not doc_requires_governance_headers(path):
        return False
    return bool(SEMANTIC_PLAN_DOC_NAME_RE.search(path.name))


def doc_requires_historical_marker(path: Path) -> bool:
    relative = repo_relative_path(path)
    return relative in RETAINED_HISTORICAL_DOCS or relative in ARCHIVED_SNAPSHOT_DOCS


def has_historical_marker(lines: list[str]) -> bool:
    for line in lines:
        stripped = line.strip()
        if any(stripped.startswith(prefix) for prefix in HISTORICAL_MARKER_PREFIXES):
            return True
        sl = stripped.lower()
        if stripped.startswith(STATUS_PREFIXES) and ("historical" in sl or "superseded" in sl):
            return True
    return False


def classify_doc_top_status(lines: list[str]) -> str:
    status_line = find_prefixed_header(lines, STATUS_PREFIXES)
    if has_historical_marker(lines):
        return "historical"
    if status_line is None:
        return "unknown"

    lowered = status_line.lower()
    if any(term in lowered for term in HISTORICAL_STATUS_TERMS):
        return "historical"
    if any(term in lowered for term in ACTIVE_STATUS_TERMS):
        return "active"
    return "unknown"


def has_bounded_close_out_context(lines: list[str], line_index: int) -> bool:
    start = max(0, line_index - 1)
    end = min(len(lines), line_index + 3)
    context = " ".join(line.strip().lower() for line in lines[start:end])
    return any(marker in context for marker in BOUNDED_CLOSE_OUT_CONTEXT_MARKERS)


def active_plan_status_drift_findings(path: Path, text: str) -> list[str]:
    if not doc_requires_semantic_plan_status_audit(path):
        return []

    top_lines = first_lines(text, 30)
    if classify_doc_top_status(top_lines) != "active":
        return []

    findings: list[str] = []
    has_current_plan_status_note = has_prefixed_header(top_lines, CURRENT_PLAN_STATUS_PREFIXES)
    relative = repo_relative_path(path)
    lines = text.splitlines()

    for zero_index, line in enumerate(lines[20:], start=20):
        stripped = line.strip()

        if NO_OPEN_PHASES_LINE_RE.match(stripped):
            findings.append(
                f"{relative}:{zero_index + 1}: active plan contains later close-out line "
                f"'{stripped}'"
            )

        if HISTORICAL_CLOSE_OUT_STATUS_RE.match(stripped) and not has_current_plan_status_note:
            findings.append(
                f"{relative}:{zero_index + 1}: active plan contains later historical close-out "
                "status but no near-top Current plan status note explains the live backlog"
            )

        if (LANDED_EXIT_CRITERIA_STATUS_RE.match(stripped)
                and not has_bounded_close_out_context(lines, zero_index)):
            findings.append(
                f"{relative}:{zero_index + 1}: active plan contains later close-out status "
                f"'{stripped}' without a nearby bounded historical note"
            )

    return findings


def docs_readme_current_plan_routing_findings(path: Path, text: str) -> list[str]:
    if repo_relative_path(path) != "docs/README.md":
        return []

    findings: list[str] = []

    for index, line in enumerate(text.splitlines(), start=1):
        lowered = line.lower()
        if not any(phrase in lowered for phrase in CURRENT_PLAN_ROUTING_PHRASES):
            continue

        for match in MARKDOWN_LINK_RE.finditer(line):
            raw_target = match.group("target").strip()
            resolved = resolve_local_target(path, raw_target)
            if resolved is None or resolved.suffix.lower() != ".md":
                continue

            target_text = resolved.read_text()
            target_status = classify_doc_top_status(first_lines(target_text, 30))
            target_findings = active_plan_status_drift_findings(resolved, target_text)
            target_relative = repo_relative_path(resolved)

            if target_status == "historical":
                findings.append(
                    f"docs/README.md:{index}: current-plan routing points at "
                    f"historical doc {target_relative}"
                )
            elif target_findings:
                findings.append(
                    f"docs/README.md:{index}: current-plan routing points at "
                    f"semantically stale doc {target_relative}"
                )

    return findings


def command_rewrite_archive_links(args: argparse.Namespace) -> int:
    files = parse_paths(args.paths, default_archive_markdown_files)
    changed_files = 0
    total_rewrites = 0

    for path in files:
        original = path.read_text()
        updated, link_rewrites = rewrite_markdown_links(path, original)
        updated, plain_path_rewrites = rewrite_plain_repo_paths(updated)
        rewrites = link_rewrites + plain_path_rewrites
        if updated != original:
            path.write_text(updated)
            changed_files += 1
            total_rewrites += rewrites

    print(
        f"Rewrote {total_rewrites} portability reference(s) across {changed_files} file(s)."
    )
    return 0


def command_docs_link_audit(args: argparse.Namespace) -> int:
    files = parse_paths(args.paths, default_markdown_files)
    findings: list[str] = []

    for path in files:
        text = path.read_text()
        for index, line in enumerate(text.splitlines(), start=1):
            if NON_PORTABLE_PATH_RE.search(line):
                findings.append(
                    f"{path.relative_to(REPO_ROOT)}:{index}: non-portable path reference"
                )

        for match in MARKDOWN_LINK_RE.finditer(text):
            if match.group("bang") == "!":
                continue
            raw_target = match.group("target").strip()
            target, _anchor = split_anchor(raw_target)
            if not target or target.startswith("#") or looks_external(target):
                continue
            if resolve_local_target(path, raw_target) is None:
                findings.append(
                    f"{path.relative_to(REPO_ROOT)}: "
                    f"unresolved local markdown link target: {raw_target}"
                )

    if findings:
        for finding in findings:
            print(finding)
        print(f"{len(findings)} documentation-link issue(s) found.")
        return 1

    print("Documentation link audit passed.")
    return 0


def command_docs_governance_audit(args: argparse.Namespace) -> int:
    files = parse_paths(args.paths, default_markdown_files)
    findings: list[str] = []

    for path in files:
        text = path.read_text()
        header_lines = first_lines(text, 12)
        marker_lines = first_lines(text, 20)
        relative = repo_relative_path(path)

        if doc_requires_governance_headers(path):
            if not has_prefixed_header(header_lines, STATUS_PREFIXES):
                findings.append(
                    f"{relative}: active architecture/remediation doc "
                    "missing top-level Status header"
                )
            if not has_prefixed_header(header_lines, DATE_PREFIXES):
                findings.append(
                    f"{relative}: active architecture/remediation doc missing top-level Date header"
                )

        if doc_requires_historical_marker(path) and not has_historical_marker(marker_lines):
            findings.append(
                f"{relative}: retained historical doc missing explicit "
                "historical snapshot/baseline marker"
            )

        findings.extend(active_plan_status_drift_findings(path, text))
        findings.extend(docs_readme_current_plan_routing_findings(path, text))

    if findings:
        for finding in findings:
            print(finding)
        print(f"{len(findings)} documentation-governance issue(s) found.")
        return 1

    print("Documentation governance audit passed.")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Portable repo-root helpers and markdown hygiene utilities."
    )
    subparsers = parser.add_subparsers(dest="command")

    audit = subparsers.add_parser(
        "docs-link-audit",
        help="Audit markdown docs for non-portable path references and broken local links.",
    )
    audit.add_argument("paths", nargs="*", help="Optional markdown files or directories to audit.")
    audit.set_defaults(func=command_docs_link_audit)

    governance = subparsers.add_parser(
        "docs-governance-audit",
        help=(
            "Audit governed docs for metadata, explicit historical markers, "
            "semantic plan-status drift, and stale current-plan routing."
        ),
    )
    governance.add_argument(
        "paths", nargs="*", help="Optional markdown files or directories to audit."
    )
    governance.set_defaults(func=command_docs_governance_audit)

    rewrite = subparsers.add_parser(
        "rewrite-archive-links",
        help="Rewrite archive markdown links and plain repo-root paths into portable forms.",
    )
    rewrite.add_argument(
        "paths",
        nargs="*",
        help=(
            "Optional archive markdown files or directories to rewrite. "
            "Defaults to docs/archive/2026-03."
        ),
    )
    rewrite.set_defaults(func=command_rewrite_archive_links)

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if not hasattr(args, "func"):
        parser.print_help()
        return 0
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
