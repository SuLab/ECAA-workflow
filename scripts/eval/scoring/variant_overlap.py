"""Variant-overlap Jaccard for the Nekrutenko mtDNA task.

A variant key is (chrom, pos, ref, alt) of a PASS (or unfiltered) record.
Multiallelic records (ALT = comma-separated alleles) are decomposed into one
key per ALT allele so each allele matches independently against a single-allele
answer key. Two shared keys match only if their AF agrees within af_tol.
Jaccard = |matched| / |union of keys|.

Two flavours:

* Per-sample (``jaccard`` / ``mean_jaccard``): one obs VCF vs one key VCF,
  paired by the caller.
* Recipe-agnostic (``flat_variant_set`` / ``flat_jaccard``): pool ALL obs VCFs
  into one call set and ALL key VCFs into another, then compare. This compares
  the CALL SETS regardless of per-sample-vs-cohort organisation or file naming,
  and excludes gVCF intermediates (by name + non-variant ALT).
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


# Mitochondrial contig aliases. The reference VCFs spell the mito contig
# `chrM`, but tools downstream rename it (`MT` for VEP/Ensembl, `NC_012920(.1)`
# / `J01415(.2)` for RefSeq/GenBank rCRS). They are the SAME molecule, so a
# variant key keyed on the raw chrom string would treat `MT:152` and `chrM:152`
# as different calls — inflating the union and breaking matches. That penalizes a
# multi-stage pipeline (which renames the contig at an annotation step) against a
# single-script arm that never renames, i.e. it is ARM-UNFAIR. Normalize every
# mito alias to the reference's `chrM` so the comparison is convention-agnostic.
_MITO_ALIASES = {
    "M", "MT", "MITO", "MTDNA", "RCRS",
    "NC_012920", "NC_012920.1", "J01415", "J01415.2",
}


def _canonical_contig(chrom: str) -> str:
    """Canonicalize a contig name for variant-key matching. Mitochondrial
    aliases (chrM / MT / M / NC_012920 / J01415, with or without a `chr`
    prefix) all map to the reference spelling `chrM`; other contigs are
    returned unchanged."""
    c = chrom.strip()
    bare = c[3:] if c[:3].lower() == "chr" else c
    if bare.upper() in _MITO_ALIASES:
        return "chrM"
    return c


# Directory names that mark a VCF as scratch / intermediate rather than a final
# deliverable call set (e.g. an annotator's contig-renamed working copy). These
# are checked RELATIVE to the run dir (see `is_scratch_vcf`) so a run dir that
# itself lives under `/tmp` — or a pytest `tmp_path` — never false-positives.
_SCRATCH_DIR_NAMES = {"tmp", ".tmp", "_tmp", "scratch", "work", "intermediate", "intermediates"}


def is_scratch_vcf(path, root) -> bool:
    """True when `path` sits under a scratch/intermediate subdir relative to
    `root` (the run/package dir). Scratch VCFs are not deliverable call sets and
    must not enter scoring — pooling them (or matching a per-sample VCF to one)
    penalizes a multi-stage pipeline that leaves intermediates against a
    single-script arm that does not. Returns False when `path` is not under
    `root` (best-effort; the caller's glob is already root-scoped)."""
    try:
        rel = Path(path).resolve().relative_to(Path(root).resolve())
    except (ValueError, OSError):
        return False
    return any(part.lower() in _SCRATCH_DIR_NAMES for part in rel.parts[:-1])


def _parse_af_field(info: str) -> list[float]:
    """Extract the AF INFO subfield as a list of per-allele allele frequencies.

    Returns one float per comma-separated AF value, or an empty list when no
    AF is present. Unparseable values fall back to 0.0 so a malformed AF never
    raises.
    """
    for kv in info.split(";"):
        if kv.startswith("AF="):
            vals: list[float] = []
            for v in kv[3:].split(","):
                try:
                    vals.append(float(v))
                except ValueError:
                    vals.append(0.0)
            return vals
    return []


# ALT values that mark a record as NON-variant (gVCF reference blocks, missing
# calls, the bcftools mpileup symbolic non-ref allele ``<*>``). These never
# represent an actual short-variant call and must not enter any pooled or
# per-sample set. Defined here so ``parse_vcf_variants`` — the single source of
# truth for variant keys, used by BOTH the headline per-sample/macro Jaccard and
# the secondary flat-pool metric — applies one consistent variant-key definition.
_NON_VARIANT_ALTS = {"<NON_REF>", "<*>", ".", ""}


def parse_vcf_variants(path: Path) -> dict[tuple[str, int, str, str], float]:
    """Parse a VCF into a {(chrom, pos, ref, alt): af} map.

    Multiallelic records (ALT = comma-separated alleles, e.g. ``T  C,G``) are
    split into one key per ALT allele so each allele matches independently
    against a single-allele answer key. Per-allele AF (``AF=0.9,0.1``) is paired
    positionally with each ALT allele; a single AF value applies to every
    allele; a missing AF yields 0.0.
    """
    variants: dict[tuple[str, int, str, str], float] = {}
    for line in _read_vcf_text(path).splitlines():
        if not line or line.startswith("#"):
            continue
        f = line.split("\t")
        if len(f) < 8:
            continue
        flt = f[6]
        # Accept only PASS or "." (unfiltered), matching the reference
        # score_run.py exactly; an empty FILTER token is unreachable on
        # bcftools/lofreq output, so excluding it has no effect on real data.
        if flt not in ("PASS", "."):
            continue
        chrom, pos, ref, alt, info = _canonical_contig(f[0]), int(f[1]), f[3], f[4], f[7]
        alts = alt.split(",")
        afs = _parse_af_field(info)
        for i, allele in enumerate(alts):
            # Drop non-variant ALT records (gVCF reference blocks, missing calls,
            # the mpileup symbolic ``<*>``) at the variant-key source so the
            # headline per-sample/macro Jaccard cannot count them as variants —
            # the secondary flat-pool path already excluded them downstream.
            if allele in _NON_VARIANT_ALTS:
                continue
            if len(afs) == len(alts):
                af = afs[i]
            elif len(afs) == 1:
                af = afs[0]
            else:
                af = 0.0
            variants[(chrom, pos, ref, allele)] = af
    return variants


def _is_gvcf_path(path: Path) -> bool:
    """Cheap heuristic: a file whose name ends in ``.g.vcf`` / ``.g.vcf.gz`` is a
    gVCF (per-sample intermediate with reference blocks), not a final call set."""
    name = Path(path).name.lower()
    return name.endswith(".g.vcf") or name.endswith(".g.vcf.gz")


def _flat_variant_map(
    paths: list[Path],
) -> dict[tuple[str, int, str, str], float]:
    """Pool a FLAT {(chrom, pos, ref, alt): af} map across a LIST of VCFs.

    Recipe-agnostic: the union of per-allele variant keys across every given
    VCF, so a single cohort VCF and a set of per-sample VCFs that encode the
    same calls compare equal. gVCF content is excluded two ways:

    * files whose name ends ``.g.vcf`` / ``.g.vcf.gz`` are skipped wholesale
      (gVCFs are intermediates, not final calls);
    * any record whose ALT is ``<NON_REF>``, ``.`` or empty is dropped even from
      a plainly-named file (defensive against gVCF content in a non-`.g.` name).

    Reuses ``parse_vcf_variants`` (per-allele split + gzip handling). On a key
    seen in more than one file the last AF wins; the AF only gates the ±tol
    match in ``flat_jaccard`` and per-sample AFs for the same call agree closely.
    """
    pooled: dict[tuple[str, int, str, str], float] = {}
    for path in paths:
        if _is_gvcf_path(path):
            continue
        for key, af in parse_vcf_variants(path).items():
            if key[3] in _NON_VARIANT_ALTS:
                continue
            pooled[key] = af
    return pooled


def flat_variant_set(paths: list[Path]) -> set[tuple[str, int, str, str]]:
    """The pooled set of (chrom, pos, ref, alt) variant keys across a LIST of
    VCFs — recipe-agnostic, gVCF/non-variant-excluding (see ``_flat_variant_map``)."""
    return set(_flat_variant_map(paths))


def flat_jaccard(
    obs_paths: list[Path], ref_paths: list[Path], af_tol: float = 0.02
) -> float:
    """Recipe-agnostic Jaccard over CALL SETS pooled across two lists of VCFs.

    Pools all observed VCFs into one variant set and all reference VCFs into
    another (``flat_variant_set`` semantics: gVCF + non-variant exclusion), then
    scores |matched| / |union| with the same AF ±``af_tol`` tolerance the
    per-sample ``jaccard`` uses. An empty union is a vacuous 1.0.
    """
    a = _flat_variant_map(obs_paths)
    b = _flat_variant_map(ref_paths)
    union = set(a) | set(b)
    if not union:
        return 1.0
    matched = sum(1 for k in (set(a) & set(b)) if abs(a[k] - b[k]) <= af_tol)
    return matched / len(union)


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


def macro_jaccard_by_sample(
    obs_paths: list[Path], ref_dir, samples, af_tol: float = 0.02
) -> tuple[float, dict[str, float]]:
    """Per-sample macro-mean Jaccard — the Nekrutenko paper's PRIMARY M3 metric
    (``score_run.py::m3_jaccard``): for each sample, match the observed VCF by
    stem substring (gVCF intermediates excluded) and compare it to
    ``{sample}.vcf.gz`` in ``ref_dir``; a sample with no observed VCF contributes
    0.0. Returns ``(macro_mean, {sample: jaccard})``.

    This is the paper-comparable companion to ``flat_jaccard`` (which pools all
    call sets and is naming/organisation-agnostic). Both are reported so the
    headline stays recipe-agnostic while the per-sample number lines up with the
    paper's M3."""
    obs_by_sample: dict[str, Path] = {}
    for p in obs_paths:
        if _is_gvcf_path(p):
            continue
        for s in samples:
            if s in Path(p).name and s not in obs_by_sample:
                obs_by_sample[s] = Path(p)
    per: dict[str, float] = {}
    for s in samples:
        key = Path(ref_dir) / f"{s}.vcf.gz"
        if s in obs_by_sample and key.exists():
            per[s] = jaccard(obs_by_sample[s], key, af_tol)
        else:
            per[s] = 0.0
    macro = sum(per.values()) / len(samples) if samples else 0.0
    return macro, per
