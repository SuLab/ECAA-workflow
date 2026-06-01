# scripts/eval/tests/test_variant_overlap.py
from pathlib import Path
from scripts.eval.scoring.variant_overlap import parse_vcf_variants, jaccard

VCF_A = """##fileformat=VCFv4.2
#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO
chrM\t150\t.\tT\tC\t.\tPASS\tAF=0.99
chrM\t410\t.\tA\tG\t.\tPASS\tAF=0.20
"""
VCF_B = """##fileformat=VCFv4.2
#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO
chrM\t150\t.\tT\tC\t.\tPASS\tAF=0.985
chrM\t999\t.\tG\tT\t.\tPASS\tAF=0.50
"""

def test_parse(tmp_path):
    p = tmp_path / "a.vcf"; p.write_text(VCF_A)
    v = parse_vcf_variants(p)
    assert ("chrM", 150, "T", "C") in v and abs(v[("chrM",150,"T","C")] - 0.99) < 1e-9

def test_jaccard_af_tolerance(tmp_path):
    a = tmp_path / "a.vcf"; a.write_text(VCF_A)
    b = tmp_path / "b.vcf"; b.write_text(VCF_B)
    # shared key (chrM,150,T,C) AFs 0.99 vs 0.985 within ±0.02 -> match.
    # union = {150,410,999} = 3; intersect = {150} = 1 -> 1/3
    assert abs(jaccard(a, b, af_tol=0.02) - (1/3)) < 1e-9

def test_jaccard_af_outside_tolerance_not_matched(tmp_path):
    a = tmp_path / "a.vcf"; a.write_text(VCF_A)
    b = tmp_path / "b.vcf"
    b.write_text(VCF_A.replace("AF=0.99", "AF=0.50"))  # 150 AF now 0.50 vs 0.99
    # 150 key present in both but AF diff 0.49 > tol -> not a match; 410 matches
    # union {150,410}=2; intersect {410}=1 -> 0.5
    assert abs(jaccard(a, b, af_tol=0.02) - 0.5) < 1e-9


def test_parse_gzipped_vcf(tmp_path):
    """Real answer-key VCFs are bgzip/gzip-compressed; parse must decompress."""
    import gzip
    from scripts.eval.scoring.variant_overlap import parse_vcf_variants
    vcf = ("##fileformat=VCFv4.2\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n"
           "chrM\t150\t.\tT\tC\t.\tPASS\tAF=0.99\n")
    p = tmp_path / "k.vcf.gz"
    p.write_bytes(gzip.compress(vcf.encode()))
    v = parse_vcf_variants(p)
    assert ("chrM", 150, "T", "C") in v and abs(v[("chrM", 150, "T", "C")] - 0.99) < 1e-9


_HDR = "##fileformat=VCFv4.2\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n"


def test_multiallelic_record_splits_into_per_allele_keys(tmp_path):
    """A multiallelic ALT 'C,G' yields one key per allele."""
    vcf = _HDR + "chrM\t152\t.\tT\tC,G\t.\tPASS\tAF=0.6,0.4\n"
    p = tmp_path / "m.vcf"; p.write_text(vcf)
    v = parse_vcf_variants(p)
    assert ("chrM", 152, "T", "C,G") not in v  # raw comma key must NOT survive
    assert ("chrM", 152, "T", "C") in v and abs(v[("chrM", 152, "T", "C")] - 0.6) < 1e-9
    assert ("chrM", 152, "T", "G") in v and abs(v[("chrM", 152, "T", "G")] - 0.4) < 1e-9


def test_multiallelic_obs_matches_two_single_allele_ref(tmp_path):
    """Observed 'T C,G' matches a ref that lists 'T C' and 'T G' separately."""
    obs = tmp_path / "obs.vcf"
    obs.write_text(_HDR + "chrM\t152\t.\tT\tC,G\t.\tPASS\tAF=0.6,0.4\n")
    ref = tmp_path / "ref.vcf"
    ref.write_text(_HDR
                   + "chrM\t152\t.\tT\tC\t.\tPASS\tAF=0.6\n"
                   + "chrM\t152\t.\tT\tG\t.\tPASS\tAF=0.4\n")
    # both alleles match within AF tol; union = {C, G} = 2, matched = 2 -> 1.0
    assert abs(jaccard(obs, ref, af_tol=0.02) - 1.0) < 1e-9


def test_multiallelic_ref_matches_two_single_allele_obs(tmp_path):
    """Symmetric: a multiallelic REFERENCE matches two single-allele observed."""
    obs = tmp_path / "obs.vcf"
    obs.write_text(_HDR
                   + "chrM\t152\t.\tT\tC\t.\tPASS\tAF=0.6\n"
                   + "chrM\t152\t.\tT\tG\t.\tPASS\tAF=0.4\n")
    ref = tmp_path / "ref.vcf"
    ref.write_text(_HDR + "chrM\t152\t.\tT\tC,G\t.\tPASS\tAF=0.6,0.4\n")
    assert abs(jaccard(obs, ref, af_tol=0.02) - 1.0) < 1e-9


def test_multiallelic_per_allele_af_tolerance(tmp_path):
    """Per-allele AF is paired positionally: one allele in-tol, one out-of-tol."""
    obs = tmp_path / "obs.vcf"
    obs.write_text(_HDR + "chrM\t152\t.\tT\tC,G\t.\tPASS\tAF=0.60,0.40\n")
    ref = tmp_path / "ref.vcf"
    # C: 0.61 vs 0.60 (within 0.02) -> match. G: 0.90 vs 0.40 (outside) -> no match.
    ref.write_text(_HDR
                   + "chrM\t152\t.\tT\tC\t.\tPASS\tAF=0.61\n"
                   + "chrM\t152\t.\tT\tG\t.\tPASS\tAF=0.90\n")
    # union {C, G} = 2; matched {C} = 1 -> 0.5
    assert abs(jaccard(obs, ref, af_tol=0.02) - 0.5) < 1e-9


def test_multiallelic_single_af_applies_to_all_alleles(tmp_path):
    """A single AF value on a multiallelic record applies to every ALT allele."""
    vcf = _HDR + "chrM\t152\t.\tT\tC,G\t.\tPASS\tAF=0.75\n"
    p = tmp_path / "s.vcf"; p.write_text(vcf)
    v = parse_vcf_variants(p)
    assert abs(v[("chrM", 152, "T", "C")] - 0.75) < 1e-9
    assert abs(v[("chrM", 152, "T", "G")] - 0.75) < 1e-9


def test_single_allele_behavior_unchanged_regression(tmp_path):
    """Existing single-allele path is byte-for-byte unchanged by the split logic."""
    a = tmp_path / "a.vcf"; a.write_text(VCF_A)
    b = tmp_path / "b.vcf"; b.write_text(VCF_B)
    va = parse_vcf_variants(a)
    assert set(va) == {("chrM", 150, "T", "C"), ("chrM", 410, "A", "G")}
    assert abs(va[("chrM", 150, "T", "C")] - 0.99) < 1e-9
    assert abs(va[("chrM", 410, "A", "G")] - 0.20) < 1e-9
    # same union/intersection arithmetic as test_jaccard_af_tolerance -> 1/3
    assert abs(jaccard(a, b, af_tol=0.02) - (1 / 3)) < 1e-9
