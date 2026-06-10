#!/usr/bin/env python3
"""measure_af_spectrum.py - deterministic post-filter AF-spectrum metrics.

Shipped into an emitted package's lib/ and run verbatim inside the bio-min
container by the agent (gated by the task spec's attributes.measurement_script
flag). Parses a VCF (gz-aware) via `bcftools query` and emits result.json keys
the validation-contract numeric_threshold assertions read. No host compute;
runs in the container. Band edges are operator-authored constants here and are
NEVER handed to the agent.

bio-min has bcftools>=1.20 but NOT pysam/cyvcf2, so we shell out to bcftools.
"""
import argparse
import json
import subprocess
import sys

# Operator-authored reference bounds (design 3.1, WS-F). SME-overridable by
# editing this pinned script; never passed to the agent as a threshold.
NOISE_FLOOR = 0.01
HOMOPLASMY_CUTOFF = 0.5


def compute_metrics(af_values, n_samples):
    """Pure metric core (unit-tested directly). af_values: list[float]."""
    af_sorted = sorted(float(x) for x in af_values)
    low_af_band_count = sum(1 for a in af_sorted if NOISE_FLOOR <= a < HOMOPLASMY_CUTOFF)
    sub_noise_floor_count = sum(1 for a in af_sorted if a < NOISE_FLOOR)
    return {
        "af_values": af_sorted,
        "variant_count": len(af_sorted),
        "n_samples": int(n_samples),
        "low_af_band_count": low_af_band_count,
        "sub_noise_floor_count": sub_noise_floor_count,
    }


def write_result(metrics, out_path):
    with open(out_path, "w") as fh:
        json.dump(metrics, fh, sort_keys=True, indent=2)
        fh.write("\n")


def _extract_af_values(vcf_path):
    """Read AF values via `bcftools query`. Falls back to INFO/AF then per-sample FORMAT/AF."""
    af_values = []
    # Try INFO/AF first, then FORMAT/AF; bcftools prints '.' for missing.
    for fmt in ("%INFO/AF\n", "[%AF\n]"):
        proc = subprocess.run(
            ["bcftools", "query", "-f", fmt, vcf_path],
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            continue
        for line in proc.stdout.splitlines():
            tok = line.strip()
            if not tok or tok == ".":
                continue
            # AF can be comma-separated for multiallelic records.
            for piece in tok.split(","):
                piece = piece.strip()
                if piece and piece != ".":
                    try:
                        af_values.append(float(piece))
                    except ValueError:
                        continue
        if af_values:
            break
    return af_values


def _count_samples(vcf_path):
    proc = subprocess.run(
        ["bcftools", "query", "-l", vcf_path],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        return 0
    return sum(1 for line in proc.stdout.splitlines() if line.strip())


def main(argv=None):
    parser = argparse.ArgumentParser(description="AF-spectrum measurement (container-run)")
    parser.add_argument("--vcf", required=True, help="path to the post-filter VCF (gz-aware)")
    parser.add_argument("--out", required=True, help="path to write result.json")
    args = parser.parse_args(argv)
    af_values = _extract_af_values(args.vcf)
    n_samples = _count_samples(args.vcf)
    metrics = compute_metrics(af_values, n_samples)
    write_result(metrics, args.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
