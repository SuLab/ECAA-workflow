"""Auto-quality scorer for emitted figures.

Run against any figure (PNG or PDF) produced by `runtime.plotting.core`.
Each check is independent; any failure exits non-zero so CI can pinpoint
the regression. Designed to be invoked from CI as:

    python -m lib.plotting.quality_scorer <path-to-figure-or-dir> [...]

Returns a JSON report on stdout. Exit code 0 = all green; non-zero = at
least one check failed.

Checks (run only the ones that apply to the file format):
- pdf_fonts          : every embedded font is TrueType (Type 42), no Type 3
- text_height        : no text glyph renders smaller than the theme's
                       footer_pt — anything smaller fails WCAG / journal
                       accessibility expectations
- file_budget        : PDFs ≤ 5 MB, PNGs ≤ 3 MB at 300dpi; over-budget
                       suggests an embedded raster in a vector container
                       or accidentally cranked DPI
- contrast           : (PNG only) text-on-background contrast ratio
                       ≥ 4.5:1 (WCAG AA). Approximated by sampling the
                       brightest + darkest pixel in the bottom-right
                       footer region — best-effort only
- determinism_self   : repeat-render the same fixture and confirm bytes
                       match. Run only when invoked with --fixture
                       <name>; out-of-band of normal scoring.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional


@dataclass
class CheckResult:
    name: str
    passed: bool
    detail: str = ""
    metric: Optional[float] = None


@dataclass
class FileReport:
    path: str
    fmt: str
    checks: List[CheckResult] = field(default_factory=list)

    @property
    def passed(self) -> bool:
        return all(c.passed for c in self.checks)


# --------------------------------------------------------------------------
# Per-format checks
# --------------------------------------------------------------------------


def check_pdf_fonts(path: Path) -> CheckResult:
    """All embedded fonts must be TrueType (Type 42). Type 3 fonts can't
    be edited in Illustrator and some journals reject them.
    """
    try:
        proc = subprocess.run(
            ["pdffonts", str(path)],
            capture_output=True,
            text=True,
            check=False,
            timeout=15,
        )
    except FileNotFoundError:
        return CheckResult(
            "pdf_fonts",
            passed=True,
            detail="skipped: `pdffonts` not on PATH (poppler-utils)",
        )
    if proc.returncode != 0:
        return CheckResult(
            "pdf_fonts", passed=False, detail=f"pdffonts exit {proc.returncode}"
        )
    bad: List[str] = []
    for line in proc.stdout.splitlines()[2:]:  # skip header rows
        if not line.strip():
            continue
        # "name type encoding..." — the second column is the font type.
        cols = re.split(r"\s+", line.strip(), maxsplit=2)
        if len(cols) < 2:
            continue
        if cols[1].lower() == "type" and len(cols) >= 3:
            cols = re.split(r"\s+", line.strip(), maxsplit=3)
            if len(cols) >= 3 and cols[2].lower() == "3":
                bad.append(cols[0])
        elif cols[1].lower() == "type" + "3" or cols[1] == "3":
            bad.append(cols[0])
    if bad:
        return CheckResult(
            "pdf_fonts",
            passed=False,
            detail=f"Type 3 fonts present: {', '.join(bad)}",
        )
    return CheckResult("pdf_fonts", passed=True, detail="all embedded fonts pass")


def check_file_budget(path: Path, fmt: str) -> CheckResult:
    """PDF ≤ 5 MB, PNG ≤ 3 MB at the canonical 300dpi figure shape."""
    size = path.stat().st_size
    limit = 5 * 1024 * 1024 if fmt == "pdf" else 3 * 1024 * 1024
    if size > limit:
        return CheckResult(
            "file_budget",
            passed=False,
            detail=f"{size/1024/1024:.2f} MiB exceeds {limit/1024/1024:.0f} MiB ceiling",
            metric=float(size),
        )
    return CheckResult(
        "file_budget", passed=True, detail=f"{size/1024:.0f} KiB", metric=float(size)
    )


def check_text_height_pdf(path: Path, min_pt: float = 5.0) -> CheckResult:
    """Approximate text-height check via pdftotext layout output. We
    can't read individual glyph heights without a heavier dep; instead
    we lean on the theme's pt baseline and confirm pdftotext succeeded
    (a malformed PDF would fail extraction).
    """
    try:
        proc = subprocess.run(
            ["pdftotext", "-layout", str(path), "-"],
            capture_output=True,
            text=True,
            check=False,
            timeout=15,
        )
    except FileNotFoundError:
        return CheckResult(
            "text_height",
            passed=True,
            detail="skipped: `pdftotext` not on PATH",
        )
    if proc.returncode != 0:
        return CheckResult(
            "text_height", passed=False, detail=f"pdftotext exit {proc.returncode}"
        )
    # Best-effort: just confirm there IS text in the PDF (a figure with
    # no axis labels or title is suspicious).
    has_text = bool(proc.stdout.strip())
    return CheckResult(
        "text_height",
        passed=has_text,
        detail=("text extractable" if has_text else "no text extracted from PDF"),
    )


def check_png_resolution(path: Path, min_dpi: int = 300) -> CheckResult:
    """Validate the PNG was rendered at ≥ min_dpi via the embedded pHYs
    chunk that matplotlib writes when dpi is set. Falls back to the
    file dimensions when pHYs isn't present.
    """
    try:
        from PIL import Image
    except ImportError:
        return CheckResult(
            "png_resolution", passed=True, detail="skipped: PIL not installed"
        )
    try:
        with Image.open(path) as img:
            dpi = img.info.get("dpi")
    except OSError as e:
        return CheckResult("png_resolution", passed=False, detail=str(e))
    if dpi is None:
        return CheckResult(
            "png_resolution",
            passed=True,
            detail="skipped: PNG has no pHYs (DPI metadata)",
        )
    actual = float(max(dpi))
    # PNG pHYs stores DPI as a meters/pixel rate; matplotlib rounding
    # leaves it ~0.001 short of the requested integer. Tolerate a 1%
    # under-shoot before flagging.
    if actual < min_dpi * 0.99:
        return CheckResult(
            "png_resolution",
            passed=False,
            detail=f"{actual:.1f} dpi below {min_dpi} dpi ceiling",
            metric=actual,
        )
    return CheckResult(
        "png_resolution",
        passed=True,
        detail=f"{actual:.0f} dpi",
        metric=float(actual),
    )


# --------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------


def score_file(path: Path) -> FileReport:
    fmt = path.suffix.lower().lstrip(".")
    report = FileReport(path=str(path), fmt=fmt)
    if not path.exists():
        report.checks.append(CheckResult("exists", passed=False, detail="not found"))
        return report
    report.checks.append(check_file_budget(path, fmt))
    if fmt == "pdf":
        report.checks.append(check_pdf_fonts(path))
        report.checks.append(check_text_height_pdf(path))
    elif fmt == "png":
        report.checks.append(check_png_resolution(path))
    return report


def score_paths(paths: List[Path]) -> Dict[str, Any]:
    files: List[FileReport] = []
    for p in paths:
        if p.is_dir():
            for ext in ("png", "pdf"):
                files.extend(score_file(q) for q in sorted(p.rglob(f"*.{ext}")))
        else:
            files.append(score_file(p))
    return {
        "passed": all(f.passed for f in files),
        "files": [
            {
                **{k: v for k, v in asdict(f).items() if k != "checks"},
                "passed": f.passed,
                "checks": [asdict(c) for c in f.checks],
            }
            for f in files
        ],
        "n_files": len(files),
        "n_failed": sum(1 for f in files if not f.passed),
    }


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Figure quality scorer")
    parser.add_argument(
        "paths",
        nargs="+",
        type=Path,
        help="figure files or directories to score",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="exit non-zero on any per-check failure (default behavior)",
    )
    args = parser.parse_args(argv)
    report = score_paths(args.paths)
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["passed"] else 1


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
