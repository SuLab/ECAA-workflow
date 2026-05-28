#!/usr/bin/env python3
"""Phase 3 migration — infer safety blocks for every atom YAML.

Inference rules (per design §4.4 + §5):
  - If atom id is in EXEC_ATOMS → Exec / GeneratedByAgent /
    ProcessIsolation / Allowlisted (Phase 14 generated-code atoms).
  - If preferred_container.network is set → move to safety.network.
  - If preferred_container.source == 'conda' or 'host' → provisioning
    = Sealed.
  - Otherwise → defaults (Compute / None{[]} / None / None /
    DeclaredOnly), which means no safety: block is written (omission
    = default, keeps YAML byte-minimal).

Usage:
  scripts/migrate_atom_safety.py --dry-run               # show planned changes
  scripts/migrate_atom_safety.py --apply                 # write changes
  scripts/migrate_atom_safety.py --name-filter alignment # filter by file-stem substring

The --name-filter is a substring match against the YAML stem (not a
modality name — atom YAMLs are named by stage, e.g. alignment.yaml,
batch_correction.yaml). Use --name-filter variant to operate on the
4 variant-* atoms, etc.
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

try:
    import yaml
except ImportError as e:
    print(f"PyYAML required: pip install pyyaml ({e})", file=sys.stderr)
    sys.exit(2)


# Atoms whose Implementation lowers to Implementation::GeneratedCode.
# Task 3.2 audits the codebase and populates this set. Migrating an
# atom into Exec safety on the wrong assumption is worse than leaving
# it Compute (the lint catches a mistake; a missing Exec is invisible).
#
# Audited against
# crates/core/src/workflow_contracts/from_atom.rs (synthesize_implementation
# produces only ManualProtocol / ContainerCommand / Unimplemented),
# crates/core/src/workflow_contracts/implementation.rs (GeneratedCode
# is reachable only once the sandbox is wired), and the 75 YAMLs in
# config/stage-atoms/ (none currently authored as GeneratedCode). The
# only live `Implementation::GeneratedCode` construction is
# plot_affordance/sandbox.rs::check_drafted_renderer for
# runtime-drafted renderers, which are synthesized at execution
# time and never lowered from a stage-atom YAML.
#
# Once GeneratedCode lowering is wired for specific atoms, populate
# this set with the affected atom ids so the migration script marks
# them with the Exec safety block.
EXEC_ATOMS: set[str] = set()


def infer_safety(atom: dict) -> dict | None:
    """Return safety block dict, or None to mean 'use defaults'.

    Defaults = Compute / None{[]} / None / None / DeclaredOnly. When
    None is returned, the migrator writes NO `safety:` block so the
    YAML stays byte-minimal (omission = default).
    """
    atom_id = atom.get("id", "")
    container = atom.get("preferred_container") or {}
    has_network = isinstance(container, dict) and "network" in container
    network_value = container.get("network") if isinstance(container, dict) else None
    source = container.get("source") if isinstance(container, dict) else None

    if isinstance(source, dict):
        source_kind = source.get("kind", "image")
    else:
        source_kind = "image"

    is_exec = atom_id in EXEC_ATOMS
    needs_sealed = source_kind != "image"

    if not is_exec and not has_network and not needs_sealed:
        return None  # defaults are correct; omit safety: block

    safety: dict = {}
    if is_exec:
        safety["level"] = "exec"
        safety["code_execution"] = "generated_by_agent"
        safety["sandbox"] = "process_isolation"
        safety["provisioning"] = "allowlisted"
        # Exec atoms get a network policy too — bridge if not specified
        if has_network:
            safety["network"] = network_value
        else:
            safety["network"] = {"kind": "bridge"}
    elif has_network:
        # Non-default network on a non-Exec atom → Network level
        safety["level"] = "network"
        safety["network"] = network_value
    elif needs_sealed:
        # Sealed-provisioning atom without network or exec → still Compute
        # but with provisioning override (rest defaults)
        safety["provisioning"] = "sealed"

    return safety


def migrate_file(path: Path, apply: bool) -> tuple[bool, str]:
    text = path.read_text()
    atom = yaml.safe_load(text)
    if atom is None:
        return (False, "empty file")
    if "safety" in atom:
        return (False, "already has safety block")
    safety = infer_safety(atom)
    if safety is None:
        return (False, "uses defaults (no safety block needed)")

    atom["safety"] = safety
    # Clear deprecated preferred_container.network if moved.
    if "network" in safety and isinstance(atom.get("preferred_container"), dict):
        atom["preferred_container"].pop("network", None)

    if apply:
        # Preserve key order; one-line flow for compactness off.
        path.write_text(
            yaml.safe_dump(atom, sort_keys=False, default_flow_style=False)
        )
    return (True, f"migrate -> level={safety.get('level', 'compute (default)')}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--apply", action="store_true", help="write changes")
    mode.add_argument("--dry-run", action="store_true", help="show planned changes")
    ap.add_argument(
        "--name-filter",
        "--modality",  # deprecated alias; kept so prior invocations don't break
        dest="name_filter",
        help="substring match against atom YAML file stem (not modality)",
    )
    ap.add_argument("--config-dir", default="config/stage-atoms",
                    help="directory of atom YAMLs (default: config/stage-atoms)")
    args = ap.parse_args()

    config_dir = Path(args.config_dir)
    if not config_dir.is_dir():
        print(f"config dir not found: {config_dir}", file=sys.stderr)
        return 2

    files = sorted(f for f in config_dir.glob("*.yaml") if not f.name.startswith("_"))
    if args.name_filter:
        files = [f for f in files if args.name_filter in f.stem]

    if not files:
        print("no atom YAMLs matched filter")
        return 0

    changes = 0
    skipped = 0
    for f in files:
        changed, note = migrate_file(f, apply=args.apply)
        status = "MIGRATE" if changed else "skip   "
        print(f"{status} {f.name}: {note}")
        if changed:
            changes += 1
        else:
            skipped += 1
    action = "applied" if args.apply else "would migrate"
    print(f"\n{changes} {action}, {skipped} skipped.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
