#!/usr/bin/env python3
"""apt / apt-get install-proxy shim.

Real apt lives at /usr/local/bin/.real/apt. This shim is mounted at
/usr/local/bin/apt and intercepts every call. Pass-through for
non-install commands (update, search, list).
"""
from __future__ import annotations
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _common  # noqa: E402


REAL = os.environ.get("SWFC_REAL_APT", "/usr/local/bin/.real/apt")
INSTALL_LOG = os.environ.get(
    "SWFC_INSTALL_LOG", "/workspace/runtime/install-log.jsonl"
)


def parse_packages(args: list[str]) -> list[str]:
    """Extract package names from `apt install ...` argv."""
    if not args or "install" not in args:
        return []
    install_idx = args.index("install")
    # After 'install', strip flags (-y, --yes, --no-install-recommends, etc.)
    rest = args[install_idx + 1:]
    return [a for a in rest if not a.startswith("-")]


def main() -> int:
    args = sys.argv[1:]

    # Non-install commands pass through.
    if not args or "install" not in args:
        return subprocess.call([REAL] + args)

    if _common.bypass_enabled():
        return subprocess.call([REAL] + args)

    pkgs = parse_packages(args)
    if not pkgs:
        return subprocess.call([REAL] + args)

    try:
        policy = _common.load_policy()
    except FileNotFoundError:
        sys.stderr.write("provisioning.json not found; exiting\n")
        return _common.EXIT_POLICY_MISSING

    for pkg in pkgs:
        decision = _common.check_allowed(policy, "apt", pkg)
        if not decision.allowed:
            _common.fail_denied(decision)  # exits 73

    # All accepted — pass through to real apt.
    rc = subprocess.call([REAL] + args)
    if rc == 0:
        for pkg in pkgs:
            _common.log_install(
                INSTALL_LOG,
                atom_id=policy.atom_id,
                package=pkg,
                registry="apt",
            )
    return rc


if __name__ == "__main__":
    sys.exit(main())
