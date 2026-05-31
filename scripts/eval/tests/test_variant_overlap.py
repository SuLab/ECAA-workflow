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
