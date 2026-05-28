#!/usr/bin/env python3
"""npm install-proxy shim.

Parses `npm install <pkg>` or `npm i <pkg>`. Bare `npm install` (no
packages) is a manifest-driven install and passes through.
"""
from __future__ import annotations
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _common  # noqa: E402


REAL = os.environ.get("ECAA_REAL_NPM", "/usr/local/bin/.real/npm")
INSTALL_LOG = os.environ.get(
    "ECAA_INSTALL_LOG", "/workspace/runtime/install-log.jsonl"
)


def parse_install(args: list[str]) -> list[str]:
    """Return packages for `npm install/i <pkg>...`. Empty list means
    manifest-driven install — that's pass-through (no new packages
    appearing without a manifest edit)."""
    cmd_idx = None
    for i, a in enumerate(args):
        if a in ("install", "i", "add"):
            cmd_idx = i
            break
    if cmd_idx is None:
        return []
    rest = args[cmd_idx + 1:]
    return [a for a in rest if not a.startswith("-")]


def main() -> int:
    args = sys.argv[1:]

    if not any(a in ("install", "i", "add") for a in args):
        return subprocess.call([REAL] + args)

    if _common.bypass_enabled():
        return subprocess.call([REAL] + args)

    pkgs = parse_install(args)
    # Empty pkg list = manifest install — pass through unchecked.
    if not pkgs:
        return subprocess.call([REAL] + args)

    try:
        policy = _common.load_policy()
    except FileNotFoundError:
        sys.stderr.write("provisioning.json not found; exiting\n")
        return _common.EXIT_POLICY_MISSING

    for pkg in pkgs:
        decision = _common.check_allowed(policy, "npm", pkg)
        if not decision.allowed:
            _common.fail_denied(decision)

    rc = subprocess.call([REAL] + args)
    if rc == 0:
        for pkg in pkgs:
            _common.log_install(
                INSTALL_LOG,
                atom_id=policy.atom_id,
                package=pkg,
                registry="npm",
            )
    return rc


if __name__ == "__main__":
    sys.exit(main())
