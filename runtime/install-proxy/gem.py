#!/usr/bin/env python3
"""gem install-proxy shim.

Parses `gem install <name>`. Registry = "rubygems".
"""
from __future__ import annotations
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _common  # noqa: E402


REAL = os.environ.get("SWFC_REAL_GEM", "/usr/local/bin/.real/gem")
INSTALL_LOG = os.environ.get(
    "SWFC_INSTALL_LOG", "/workspace/runtime/install-log.jsonl"
)


def parse_install(args: list[str]) -> list[str]:
    if "install" not in args:
        return []
    install_idx = args.index("install")
    return [a for a in args[install_idx + 1:] if not a.startswith("-")]


def main() -> int:
    args = sys.argv[1:]
    if "install" not in args:
        return subprocess.call([REAL] + args)
    if _common.bypass_enabled():
        return subprocess.call([REAL] + args)
    pkgs = parse_install(args)
    if not pkgs:
        return subprocess.call([REAL] + args)

    try:
        policy = _common.load_policy()
    except FileNotFoundError:
        sys.stderr.write("provisioning.json not found; exiting\n")
        return _common.EXIT_POLICY_MISSING

    for pkg in pkgs:
        decision = _common.check_allowed(policy, "rubygems", pkg)
        if not decision.allowed:
            _common.fail_denied(decision)

    rc = subprocess.call([REAL] + args)
    if rc == 0:
        for pkg in pkgs:
            _common.log_install(
                INSTALL_LOG,
                atom_id=policy.atom_id,
                package=pkg,
                registry="rubygems",
            )
    return rc


if __name__ == "__main__":
    sys.exit(main())
