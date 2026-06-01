"""Per-sample variant-overlap Jaccard for the Nekrutenko mtDNA task.

A variant key is (chrom, pos, ref, alt) of a PASS (or unfiltered) record.
Two shared keys match only if their AF agrees within af_tol. Jaccard =
|matched| / |union of keys|.
"""
from __future__ import annotations
import gzip
from pathlib import Path


def _read_vcf_text(path: Path) -> str:
    """Read a VCF as text, transparently decompressing bgzip/gzip (.vcf.gz).

    Real Nekrutenko answer-key VCFs are bgzip-compressed; agent outputs may be
    plain .vcf. Sniff the gzip magic bytes rather than trusting the extension.
    """
    p = Path(path)
    with open(p, "rb") as fh:
        magic = fh.read(2)
    if magic == b"\x1f\x8b":
        with gzip.open(p, "rt") as fh:
            return fh.read()
    return p.read_text()


def parse_vcf_variants(path: Path) -> dict[tuple[str, int, str, str], float]:
    variants: dict[tuple[str, int, str, str], float] = {}
    for line in _read_vcf_text(path).splitlines():
        if not line or line.startswith("#"):
            continue
        f = line.split("\t")
        if len(f) < 8:
            continue
        flt = f[6]
        if flt not in ("PASS", ".", ""):
            continue
        chrom, pos, ref, alt, info = f[0], int(f[1]), f[3], f[4], f[7]
        af = 0.0
        for kv in info.split(";"):
            if kv.startswith("AF="):
                try:
                    af = float(kv[3:].split(",")[0])
                except ValueError:
                    af = 0.0
        variants[(chrom, pos, ref, alt)] = af
    return variants


def jaccard(obs: Path, key: Path, af_tol: float = 0.02) -> float:
    a = parse_vcf_variants(obs)
    b = parse_vcf_variants(key)
    union = set(a) | set(b)
    if not union:
        return 1.0
    matched = sum(1 for k in (set(a) & set(b)) if abs(a[k] - b[k]) <= af_tol)
    return matched / len(union)


def mean_jaccard(sample_pairs: list[tuple[Path, Path]], af_tol: float = 0.02) -> float:
    if not sample_pairs:
        return 0.0
    return sum(jaccard(o, k, af_tol) for o, k in sample_pairs) / len(sample_pairs)
