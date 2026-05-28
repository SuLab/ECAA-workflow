#!/usr/bin/env python3
"""pip install-proxy shim.

Parses `pip install <pkg> [--index-url URL]`. Registry = "pip" unless
--index-url specifies a non-default index, in which case registry is
that URL.
"""
from __future__ import annotations
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _common  # noqa: E402


REAL = os.environ.get("SWFC_REAL_PIP", "/usr/local/bin/.real/pip")
INSTALL_LOG = os.environ.get(
    "SWFC_INSTALL_LOG", "/workspace/runtime/install-log.jsonl"
)
DEFAULT_PYPI = "https://pypi.org/simple"


def parse_install(args: list[str]) -> tuple[str, list[str]]:
    """Return (registry, packages) for `pip install ...`. Registry is
    "pip" for the default index, or the --index-url value otherwise."""
    if "install" not in args:
        return ("pip", [])
    install_idx = args.index("install")
    rest = args[install_idx + 1:]

    registry = "pip"
    pkgs: list[str] = []
    i = 0
    while i < len(rest):
        a = rest[i]
        if a in ("--index-url", "-i"):
            if i + 1 < len(rest):
                val = rest[i + 1]
                if val != DEFAULT_PYPI:
                    registry = val
                i += 2
                continue
        if a.startswith("--index-url="):
            val = a.split("=", 1)[1]
            if val != DEFAULT_PYPI:
                registry = val
            i += 1
            continue
        if a.startswith("-"):
            i += 1
            continue
        pkgs.append(a)
        i += 1
    return (registry, pkgs)


def main() -> int:
    args = sys.argv[1:]

    if not args or "install" not in args:
        return subprocess.call([REAL] + args)

    if _common.bypass_enabled():
        return subprocess.call([REAL] + args)

    registry, pkgs = parse_install(args)
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
