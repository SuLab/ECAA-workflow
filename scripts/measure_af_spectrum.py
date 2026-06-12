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


def compute_metrics(af_values, n_samples, per_sample_counts=None):
    """Pure metric core (unit-tested directly). af_values: list[float].

    `per_sample_counts`: list[int] of variant counts per FORMAT sample (from
    _per_sample_counts). When None/empty — the lofreq INFO-only / sample-less
    regime where the VCF carries no FORMAT sample columns — the pooled count is
    attributed to one effective sample as a single-element array [variant_count].
    """
    af_sorted = sorted(float(x) for x in af_values)
    variant_count = len(af_sorted)
    low_af_band_count = sum(1 for a in af_sorted if NOISE_FLOOR <= a < HOMOPLASMY_CUTOFF)
    sub_noise_floor_count = sum(1 for a in af_sorted if a < NOISE_FLOOR)
    # A VCF carrying variant records represents at least one sample even when it
    # is sample-less in the header — lofreq writes AF to INFO with NO FORMAT
    # sample column, so `bcftools query -l` returns 0. Floor at 1 (only when
    # there ARE records) so the cross-stage `sample_count_consistent` /
    # `sample_count_recorded` (>=1) assertions read a well-defined value rather
    # than 0, and the per-sample rate below never divides by zero.
    n = max(int(n_samples), 1) if variant_count else int(n_samples)
    # Per-sample variant counts as an ARRAY — the shape the
    # `*_variant_count_per_sample_in_range` reference_range_outlier assertions
    # read (the harness checks EACH sample's count against the reference band).
    # Single-element pooled fallback for sample-less VCFs (one effective sample).
    per_sample = [int(c) for c in per_sample_counts] if per_sample_counts else (
        [variant_count] if variant_count else []
    )
    return {
        "af_values": af_sorted,
        "variant_count": variant_count,
        "n_samples": n,
        # ARRAY of per-sample variant counts read by the reference-range
        # assertions; single-element [variant_count] when the VCF has no FORMAT
        # sample columns (lofreq INFO-only).
        "variant_count_per_sample": per_sample,
        # Pooled scalar rate, informational only (NOT read by any assertion).
        "variant_count_per_sample_mean": round(variant_count / n, 4) if n else 0.0,
        # Minimum surviving allele frequency (informational only). We deliberately
        # do NOT emit a `min_surviving_af_meets_declared_threshold` pass/fail flag:
        # defining it as `sub_noise_floor_count == 0` would be TAUTOLOGICAL with the
        # no_sub_noise_floor_calls assertion (a self-satisfying check = gaming), and
        # this script cannot know the AGENT'S declared filter threshold. The
        # noise-floor invariant is enforced directly by no_sub_noise_floor_calls.
        "min_surviving_af": af_sorted[0] if af_sorted else 0.0,
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


def _per_sample_counts(vcf_path):
    """Per-FORMAT-sample called-variant counts, in header order. Empty list when
    the VCF has no FORMAT sample columns (lofreq INFO-only) — compute_metrics then
    falls back to a single-element pooled array. Counts records where the sample
    carries a called ALT genotype (GT="alt"), the standard per-sample call tally."""
    names_proc = subprocess.run(
        ["bcftools", "query", "-l", vcf_path], capture_output=True, text=True
    )
    if names_proc.returncode != 0:
        return []
    names = [s.strip() for s in names_proc.stdout.splitlines() if s.strip()]
    if not names:
        return []
    counts = {n: 0 for n in names}
    proc = subprocess.run(
        ["bcftools", "query", "-f", "[%SAMPLE\n]", "-i", 'GT="alt"', vcf_path],
        capture_output=True,
        text=True,
    )
    if proc.returncode == 0:
        for line in proc.stdout.splitlines():
            s = line.strip()
            if s in counts:
                counts[s] += 1
    return [counts[n] for n in names]


def main(argv=None):
    parser = argparse.ArgumentParser(description="AF-spectrum measurement (container-run)")
    parser.add_argument("--vcf", required=True, help="path to the post-filter VCF (gz-aware)")
    parser.add_argument("--out", required=True, help="path to write result.json")
    args = parser.parse_args(argv)
    af_values = _extract_af_values(args.vcf)
    n_samples = _count_samples(args.vcf)
    per_sample = _per_sample_counts(args.vcf)
    metrics = compute_metrics(af_values, n_samples, per_sample)
    write_result(metrics, args.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
