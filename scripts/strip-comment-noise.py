#!/usr/bin/env python3
"""Strip patch-note prefixes from comments without altering code semantics.

Run with `--dry-run` to preview; `--apply` to write changes.

The script is intentionally conservative:
  - It only ever modifies lines that are themselves comments.
  - It strips PREFIX tokens (Phase N —, Plan §X.Y —, RC-17 —, F-X-Y —,
    (YYYY-MM-DD)) but never deletes substantive content after a prefix.
  - It deletes lines that are PURE noise (the only content is the prefix
    tokens, < 30 chars after stripping).
  - It honours the allowlist in `.comment-hygiene-allowlist.toml`.

Usage:
    scripts/strip-comment-noise.py --dry-run
    scripts/strip-comment-noise.py --apply
    scripts/strip-comment-noise.py --apply --path crates/core/src/dag.rs
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST_PATH = REPO_ROOT / ".comment-hygiene-allowlist.toml"


def parse_allowlist() -> tuple[list[str], list[re.Pattern[str]]]:
    """Return (file_globs, pattern_exemptions) from the TOML allowlist."""
    if not ALLOWLIST_PATH.exists():
        return [], []
    text = ALLOWLIST_PATH.read_text()
    globs = re.findall(r'^\s*glob\s*=\s*"([^"]+)"', text, re.MULTILINE)
    patterns = re.findall(
        r"""^\s*pattern\s*=\s*['"]([^'"]+)['"]""", text, re.MULTILINE
    )
    compiled = [re.compile(p) for p in patterns]
    return globs, compiled


def _glob_to_regex(g: str) -> re.Pattern[str]:
    """Translate our limited glob syntax to a regex.

    Rules:
      `**`  -> any sequence (including slashes)
      `*`   -> any character except `/`
      `?`   -> any single character except `/`
      everything else is escaped literally
    """
    out = []
    i = 0
    while i < len(g):
        c = g[i]
        if c == "*" and i + 1 < len(g) and g[i + 1] == "*":
            out.append(".*")
            i += 2
        elif c == "*":
            out.append(r"[^/]*")
            i += 1
        elif c == "?":
            out.append(r"[^/]")
            i += 1
        else:
            out.append(re.escape(c))
            i += 1
    return re.compile("^" + "".join(out) + "$")


_GLOB_CACHE: dict[str, re.Pattern[str]] = {}


def is_file_exempt(rel_path: str, globs: list[str]) -> bool:
    for g in globs:
        pat = _GLOB_CACHE.get(g)
        if pat is None:
            pat = _glob_to_regex(g)
            _GLOB_CACHE[g] = pat
        if pat.match(rel_path):
            return True
    return False


# ---------------------------------------------------------------- patterns

# Token patterns we strip when they appear as prefixes in comments.
PREFIX_TOKENS = [
    # Plan-section refs with optional Phase/Task subsidiary
    re.compile(
        r'Plan \xa7[A-Za-z]?\.?[A-Za-z]?\d+(\.\d+)*'
        r'(\s+(Phase|Task|Step)\s+[A-Z]?\d*(\.\d+)?)?'
        r'(\s*\([^)]*\))?\s*[—:-]\s*'
    ),
    # Phase N / Phase N Task M / Phase N of the X plan / Phase N follow-up
    # Allow hyphens/digits in the "of the X plan" clause (value-add, real-value, etc.)
    re.compile(
        r'\bPhase \d+[a-z]?(\.\d+)?'
        r'(\s+(Task|Step|Work item)\s+[A-Z]?\d*(\.\d+)?[a-z]?)?'
        r'(\s+of the [\w\s.,-]+? plan)?'
        r'(\s+follow-up)?'
        r'(\s*\([^)]*\))?\s*[—:-]\s*'
    ),
    # v3/v4 Phase N variants
    re.compile(
        r'\bv\d+(\+v\d+)?[A-Za-z ]*Phase \d+(\.\d+)?'
        r'(\s+(Task|Step)\s+[A-Z]?\d*(\.\d+)?)?\s*[—:-]\s*'
    ),
    # RC-N tickets (with optional / Phase suffix)
    re.compile(r'\bRC-\d+(\s*/\s*[A-Za-z0-9 .]+)?\s*[—:-]\s*'),
    # F-X-Y-Z feature IDs
    re.compile(r'\bF-[A-Z][A-Z]+(-[A-Z0-9]+)*(\s*\([^)]*\))?\s*[—:-]\s*'),
    # v3 §N.M variant
    re.compile(r'\bv\d+ \xa7[\d.]+(\s*\([^)]*\))?\s*[—:-]\s*'),
    # Stage N / Wave N / Round N / Iteration N / Milestone N / Sprint N
    re.compile(r'\b(Stage|Wave|Round|Iteration|Milestone|Sprint) \d+(\.\d+)?\s*[—:-]\s*'),
    # Pre-fix / Post-fix prefix markers
    re.compile(r'\b(Pre-fix|Post-fix)\b\s*[:—-]?\s*'),
    # Task N of the YYYY-MM-DD plan
    re.compile(r'Task \d+ of the \d{4}-\d{2}-\d{2}[^—:.]*[—:.]?\s*'),
    # Bare F1 / D5 prefix at start
    re.compile(r'^(?=\S)[FD]\d{1,3}\s*[—:-]\s*'),
]

# Inline date suffix to strip: "" or ",)"
DATE_SUFFIX = re.compile(r'\s*\(\d{4}-\d{2}-\d{2}\)')
# Bare-prefix date strip: "foo" -> "foo". Guarded so that
# dates inside paths/filenames (preceded by - or / or followed by / or -)
# are never touched.
INLINE_DATE = re.compile(
    r'(?<![-/\w])\b\d{4}-\d{2}-\d{2}\b(?![-/.\w])\s*[—:-]?\s*'
)
# If the line still contains a date that looks like part of a path
# (foo-2026-05-07.md or docs/2026-05/foo.md), skip date stripping entirely.
PATH_DATE_GUARD = re.compile(r'[/\w-]\d{4}-\d{2}-\d{2}[/\w.-]')

# Migration prose tokens to soft-strip from prose.
MIGRATION_PROSE = re.compile(
    r'\b(landed|shipped|cutover|post-cutover|pre-B\d+)\b'
)

# Comment-line detection per file extension.
COMMENT_RE = {
    "rs": re.compile(r'^(\s*)(//[!/]?|/\*[!*]?\s*|\s*\*\s?)'),
    "ts": re.compile(r'^(\s*)(//[!/]?|/\*[!*]?\s*|\s*\*\s?)'),
    "tsx": re.compile(r'^(\s*)(//[!/]?|/\*[!*]?\s*|\s*\*\s?)'),
    "py": re.compile(r'^(\s*)(#\s?)'),
    "sh": re.compile(r'^(\s*)(#\s?)'),
    "yaml": re.compile(r'^(\s*)(#\s?)'),
    "yml": re.compile(r'^(\s*)(#\s?)'),
    "toml": re.compile(r'^(\s*)(#\s?)'),
}


def get_ext(path: Path) -> str:
    if path.name.startswith("Makefile"):
        return "sh"
    return path.suffix.lstrip(".")


def strip_line(line: str, ext: str, pattern_exemptions: list[re.Pattern[str]]) -> tuple[str, bool]:
    """Return (new_line, was_modified).

    A returned new_line == "" with was_modified == True means: delete this line.
    """
    # Quick exit: only process comment lines.
    com_re = COMMENT_RE.get(ext)
    if not com_re:
        return line, False
    m = com_re.match(line)
    if not m:
        # Could be an inline comment after code (e.g., `let x = 5; // Phase 6`)
        # For safety, skip inline comments — modifying them risks breaking code.
        return line, False

    # Allowlisted patterns: if line matches an exemption pattern, don't touch.
    for exempt in pattern_exemptions:
        if exempt.search(line):
            return line, False

    indent, prefix = m.group(1), m.group(2)
    body = line[m.end():].rstrip("\n")
    original_body = body

    # Skip already-blank comment lines (`///`, `//!`, `#` alone). These are
    # paragraph separators in rustdoc; deleting them would merge paragraphs.
    if original_body.strip() == "":
        return line, False

    # Apply transformations.
    # 1. Strip inline date parenthetical: "X" -> "X"
    body = DATE_SUFFIX.sub('', body)
    # 2. Strip prefix tokens.
    for pat in PREFIX_TOKENS:
        body = pat.sub('', body, count=1)
    # 3. Strip leading bare date when not part of a filename/path.
    if not PATH_DATE_GUARD.search(body):
        body = INLINE_DATE.sub('', body, count=1)
    # 4. Strip standalone migration tokens within prose. Drop the
    #  Leading whitespace so "Foo X." -> "Foo X." cleanly.
    body = re.sub(r'\s+(landed|shipped|cutover)\b(?=[.,;:)]|\s*$)', '', body)
    # 5. Collapse double spaces left by stripping; trim trailing
    #  whitespace-before-punctuation.
    body = re.sub(r'  +', ' ', body)
    body = re.sub(r'\s+([.,;:)])', r'\1', body)

    # If the body lost all real content, delete the line entirely.
    stripped = body.strip()
    if not stripped:
        return "", True
    if len(stripped) < 5 and original_body.strip() != stripped:
        return "", True

    # Capitalize the first letter if we stripped a prefix and the result now
    # starts with a lowercase letter (the prefix had been the capitalizer).
    if original_body.lstrip() != body.lstrip():
        first_char_idx = len(body) - len(body.lstrip())
        if first_char_idx < len(body) and body[first_char_idx].islower():
            body = body[:first_char_idx] + body[first_char_idx].upper() + body[first_char_idx + 1:]

    new_line = f"{indent}{prefix}{body}\n"
    return new_line, new_line != line


def collect_files(target_paths: list[Path]) -> list[Path]:
    """Return list of files under `target_paths` git-tracked, with supported extensions."""
    cmd = ["git", "ls-files", "--cached", "--others", "--exclude-standard"]
    cmd += [str(p) for p in target_paths]
    try:
        out = subprocess.check_output(cmd, cwd=REPO_ROOT, text=True)
    except subprocess.CalledProcessError:
        return []
    supported = {"rs", "ts", "tsx", "py", "sh", "yaml", "yml", "toml"}
    files = []
    for line in out.splitlines():
        p = REPO_ROOT / line
        if not p.is_file():
            continue
        ext = get_ext(p)
        if ext in supported or p.name.startswith("Makefile"):
            files.append(p)
    return files


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true", help="Write changes (default: dry-run)")
    parser.add_argument("--dry-run", action="store_true", help="Preview without writing")
    parser.add_argument(
        "--path", action="append", default=[], help="Limit to these paths (repeatable)"
    )
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()

    if not args.apply and not args.dry_run:
        args.dry_run = True

    globs, pattern_exemptions = parse_allowlist()

    target_paths = [Path(p) for p in args.path] if args.path else [Path(".")]
    files = collect_files(target_paths)

    total_modified = 0
    total_deleted_lines = 0
    file_summary: list[tuple[str, int, int]] = []  # (path, lines_modified, lines_deleted)

    for f in files:
        rel = f.relative_to(REPO_ROOT)
        rel_str = str(rel)
        if is_file_exempt(rel_str, globs):
            continue
        try:
            original = f.read_text()
        except UnicodeDecodeError:
            continue

        ext = get_ext(f)
        out_lines: list[str] = []
        lines_modified = 0
        lines_deleted = 0
        for line in original.splitlines(keepends=True):
            new_line, modified = strip_line(line, ext, pattern_exemptions)
            if modified:
                if new_line == "":
                    lines_deleted += 1
                else:
                    lines_modified += 1
                    out_lines.append(new_line)
            else:
                out_lines.append(line)

        new_text = "".join(out_lines)
        if new_text == original:
            continue

        file_summary.append((rel_str, lines_modified, lines_deleted))
        total_modified += lines_modified
        total_deleted_lines += lines_deleted

        if args.apply:
            f.write_text(new_text)
        if args.verbose:
            print(f"  {rel_str}: -{lines_deleted} lines, ~{lines_modified} modified")

    # Summary.
    print(f"Files touched: {len(file_summary)}")
    print(f"Lines modified: {total_modified}")
    print(f"Lines deleted: {total_deleted_lines}")
    if file_summary and not args.verbose:
        print("\nTop 20 files by lines changed:")
        for rel, mod, deleted in sorted(file_summary, key=lambda x: -(x[1] + x[2]))[:20]:
            print(f"  {rel}: -{deleted} +{mod}")

    if args.dry_run and file_summary:
        print("\n(dry-run; rerun with --apply to write)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
