"""collect() must index bgzipped .vcf.gz agent outputs (the answer keys are
.vcf.gz; lofreq commonly emits .vcf.gz) so score()'s stem-match can pair them."""
from scripts.eval.plugins.nekrutenko import Nekrutenko
from scripts.eval.benchmark import Arm, RunSpec


def test_collect_indexes_vcf_gz_and_plain(tmp_path):
    (tmp_path / "M117-bl.vcf.gz").write_bytes(b"\x1f\x8b\x08")  # gzip magic stub
    (tmp_path / "M117-ch.vcf").write_text("##fileformat=VCFv4.2\nchrM\t1\t.\tA\tT\n")
    spec = RunSpec(Arm.ECAA_WORKFLOW, tmp_path, "ecaa_package", "x")
    out = Nekrutenko().collect(spec, tmp_path)
    v = out.artifacts["vcfs"]
    # .vcf.gz indexed under its own name AND the plain-.vcf stem (what score() looks up)
    assert "M117-bl.vcf.gz" in v
    assert "M117-bl.vcf" in v
    # plain .vcf still indexed
    assert "M117-ch.vcf" in v


def test_collect_plain_vcf_wins_over_gz(tmp_path):
    (tmp_path / "s.vcf").write_text("##\nchrM\t1\t.\tA\tT\n")
    (tmp_path / "s.vcf.gz").write_bytes(b"\x1f\x8b\x08")
    out = Nekrutenko().collect(RunSpec(Arm.ECAA_WORKFLOW, tmp_path, "ecaa_package", "x"), tmp_path)
    assert out.artifacts["vcfs"]["s.vcf"].suffix == ".vcf"  # plain .vcf kept, not shadowed
