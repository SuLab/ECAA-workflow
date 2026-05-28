"""Shared helpers for install-proxy shim binaries.

Each package-manager shim (apt, pip, conda, ...) imports from this
module to load the per-task provisioning policy, decide whether to
allow or deny an install, and log accepted installs.

"""
from __future__ import annotations

import dataclasses
import json
import os
import sys
import time
from pathlib import Path


@dataclasses.dataclass
class Policy:
    """Per-task install-time policy. Read from
    /etc/scripps-workflow/provisioning.json by default (or wherever
    ECAA_PROVISIONING_POLICY points)."""

    provisioning: str  # "sealed" | "declared_only" | "allowlisted"
    atom_id: str
    declared_packages: dict[str, list[str]]  # registry -> declared packages
    allowed_registries: list[str]


@dataclasses.dataclass
class Decision:
    """Result of `check_allowed` — fed to either `fail_denied` or the
    real package manager."""

    allowed: bool
    reason: str


# Exit codes used by the shims.
EXIT_OK = 0
EXIT_DENIED = 73       # custom; distinguishes provisioning deny from real install failures
EXIT_POLICY_MISSING = 74


def _policy_path_default() -> str:
    """Resolve the policy file location. ECAA_PROVISIONING_POLICY
    overrides the default `/etc/scripps-workflow/provisioning.json`."""

    return os.environ.get(
        "ECAA_PROVISIONING_POLICY",
        "/etc/scripps-workflow/provisioning.json",
    )


def load_policy(path: str | None = None) -> Policy:
    """Read the per-task policy. Raises FileNotFoundError if missing.

    Caller can pass `path` explicitly; otherwise honor the
    ECAA_PROVISIONING_POLICY env override, falling back to the
    canonical /etc location.
    """
    resolved = path or _policy_path_default()
    data = json.loads(Path(resolved).read_text())
    return Policy(
        provisioning=data["provisioning"],
        atom_id=data["atom_id"],
        declared_packages=data.get("declared_packages", {}),
        allowed_registries=data.get("allowed_registries", []),
    )


def check_allowed(policy: Policy, registry: str, package: str) -> Decision:
    """Apply the policy to a single (registry, package) install request.

    Sealed     -> always deny
    DeclaredOnly -> allow iff package is in policy.declared_packages[registry]
    Allowlisted  -> allow iff registry is in policy.allowed_registries
    """
    if policy.provisioning == "sealed":
        return Decision(
            allowed=False,
            reason=f"image is sealed; install of {package} from {registry} denied",
        )
    if policy.provisioning == "declared_only":
        if package in policy.declared_packages.get(registry, []):
            return Decision(allowed=True, reason="declared")
        return Decision(
            allowed=False,
            reason=f"{package} not in atom.runtime_packages[{registry}]",
        )
    if policy.provisioning == "allowlisted":
        if registry not in policy.allowed_registries:
            return Decision(
                allowed=False,
                reason=f"registry {registry} not in allowed_registries",
            )
        return Decision(allowed=True, reason="allowlisted")
    return Decision(allowed=False, reason=f"unknown provisioning policy: {policy.provisioning}")


def log_install(
    path: str,
    *,
    atom_id: str,
    package: str,
    registry: str,
) -> None:
    """Append an accepted install to runtime/install-log.jsonl.

    JSONL — one record per line — with timestamp, atom_id, package,
    registry, and source=agent_runtime so the RO-Crate post-processing
    can distinguish runtime installs from compile-time-vendored ones.
    """
    entry = {
        "timestamp": time.time(),
        "atom_id": atom_id,
        "package": package,
        "registry": registry,
        "source": "agent_runtime",
    }
    with open(path, "a") as f:
        f.write(json.dumps(entry) + "\n")


def fail_denied(decision: Decision) -> None:
    """Print a structured error to stdout and exit with EXIT_DENIED.

    Stdout (not stderr) because the harness reads the structured marker
    from the agent's task-result envelope, which captures stdout.
    """
    print(
        json.dumps({"error": "provisioning_denied", "reason": decision.reason}),
        flush=True,
    )
    sys.exit(EXIT_DENIED)


def bypass_enabled() -> bool:
    """ECAA_PROVISIONING_DISABLE=1 bypasses the shim entirely
    (testing / debugging only). When set, the shim invokes the real
    package manager directly with no policy check."""
    return os.environ.get("ECAA_PROVISIONING_DISABLE") == "1"
