#!/usr/bin/env python3
"""conda / mamba install-proxy shim.

Parses `conda install [-c CHANNEL] <pkg> [...]`. Registry is the
channel (default "defaults"); -c bioconda → "bioconda", etc.
"""
from __future__ import annotations
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _common  # noqa: E402


def real_binary() -> str:
    """Pick the real binary based on which name we were invoked as."""
    invoked = Path(sys.argv[0]).name
    if invoked == "mamba":
        return os.environ.get("ECAA_REAL_MAMBA", "/usr/local/bin/.real/mamba")
    return os.environ.get("ECAA_REAL_CONDA", "/usr/local/bin/.real/conda")


INSTALL_LOG = os.environ.get(
    "ECAA_INSTALL_LOG", "/workspace/runtime/install-log.jsonl"
)


def parse_install(args: list[str]) -> tuple[str, list[str]]:
    """Return (registry, packages) for `conda install ...`."""
    if "install" not in args:
        return ("conda-forge", [])
    install_idx = args.index("install")
    rest = args[install_idx + 1:]

    channel = "defaults"
    pkgs: list[str] = []
    i = 0
    while i < len(rest):
        a = rest[i]
        if a == "-c" or a == "--channel":
            if i + 1 < len(rest):
                channel = rest[i + 1]
                i += 2
                continue
        if a.startswith("-c="):
            channel = a.split("=", 1)[1]
            i += 1
            continue
        if a.startswith("-"):
            i += 1
            continue
        # Conda package specs are like "samtools=1.17" — keep full spec
        # for pass-through but extract bare name for policy check.
        pkgs.append(a.split("=")[0])
        i += 1
    return (channel, pkgs)


def main() -> int:
    args = sys.argv[1:]
    REAL = real_binary()

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
