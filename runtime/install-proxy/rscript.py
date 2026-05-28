#!/usr/bin/env python3
"""Rscript install-proxy shim.

Intercepts `Rscript -e 'install.packages("X")'`. Parses the -e script
for install.packages() calls and applies policy. Other Rscript usage
passes through.
"""
from __future__ import annotations
import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _common  # noqa: E402


REAL = os.environ.get("ECAA_REAL_RSCRIPT", "/usr/local/bin/.real/Rscript")
INSTALL_LOG = os.environ.get(
    "ECAA_INSTALL_LOG", "/workspace/runtime/install-log.jsonl"
)

# Capture package name(s) from install.packages("foo") OR
# install.packages(c("a","b")) OR BiocManager::install("foo")
_RE_SINGLE = re.compile(
    r'(?:install\.packages|BiocManager::install)\s*\(\s*[\'"]([^\'"]+)[\'"]'
)
_RE_LIST = re.compile(
    r'(?:install\.packages|BiocManager::install)\s*\(\s*c\s*\(([^)]+)\)'
)


def parse_install_packages(script: str) -> tuple[str, list[str]]:
    """Return (registry, package_names) parsed from R script source."""
    # Registry: BiocManager → "bioconductor", otherwise "cran"
    registry = "bioconductor" if "BiocManager" in script else "cran"

    pkgs: list[str] = []
    for m in _RE_LIST.finditer(script):
        items = m.group(1)
        for q in re.findall(r'[\'"]([^\'"]+)[\'"]', items):
            pkgs.append(q)
    for m in _RE_SINGLE.finditer(script):
        pkgs.append(m.group(1))

    return (registry, pkgs)


def main() -> int:
    args = sys.argv[1:]

    # Only intercept when -e is used. Other invocations pass through.
    if "-e" not in args:
        return subprocess.call([REAL] + args)

    if _common.bypass_enabled():
        return subprocess.call([REAL] + args)

    e_idx = args.index("-e")
    if e_idx + 1 >= len(args):
        return subprocess.call([REAL] + args)
    script = args[e_idx + 1]

    registry, pkgs = parse_install_packages(script)
    if not pkgs:
        return subprocess.call([REAL] + args)

    try:
        policy = _common.load_policy()
    except FileNotFoundError:
        sys.stderr.write("provisioning.json not found; exiting\n")
        return _common.EXIT_POLICY_MISSING

    for pkg in pkgs:
        decision = _common.check_allowed(policy, registry, pkg)
        if not decision.allowed:
            _common.fail_denied(decision)

    rc = subprocess.call([REAL] + args)
    if rc == 0:
        for pkg in pkgs:
            _common.log_install(
                INSTALL_LOG,
                atom_id=policy.atom_id,
                package=pkg,
                registry=registry,
            )
    return rc


if __name__ == "__main__":
    sys.exit(main())
