"""collect() must index bgzipped .vcf.gz agent outputs (the answer keys are
.vcf.gz; lofreq commonly emits .vcf.gz) so score()'s flat-set pooling sees them."""
import gzip
from scripts.eval.plugins.nekrutenko import Nekrutenko
from scripts.eval.benchmark import Arm, RunSpec, Output, Task


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


_HDR = ("##fileformat=VCFv4.2\n"
        "#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n")


def _write_key(d, name, body):
    p = d / name
    p.write_bytes(gzip.compress((_HDR + body).encode()))
    return p


def test_score_cohort_vcf_matches_per_sample_answer_key(tmp_path):
    """The recipe-agnostic score: a GATK germline COHORT vcf carrying the union
    of all per-sample answer-key variants scores 1.0 even though no filename or
    per-sample organisation matches the lofreq answer key."""
    key_dir = tmp_path / "ground_truth"; key_dir.mkdir()
    _write_key(key_dir, "M117-bl.vcf.gz",  "chrM\t150\t.\tT\tC\t.\tPASS\tAF=0.99\n")
    _write_key(key_dir, "M117-ch.vcf.gz",  "chrM\t410\t.\tA\tG\t.\tPASS\tAF=0.20\n")
    _write_key(key_dir, "M117C1-bl.vcf.gz", "chrM\t600\t.\tC\tT\t.\tPASS\tAF=0.50\n")
    _write_key(key_dir, "M117C1-ch.vcf.gz", "chrM\t700\t.\tG\tA\t.\tPASS\tAF=0.30\n")

    run_dir = tmp_path / "run"; run_dir.mkdir()
    # GATK germline arm: one cohort VCF (union of all calls) + intermediate gVCFs.
    _write_key(run_dir, "cohort.filtered.vcf.gz",
               "chrM\t150\t.\tT\tC\t.\tPASS\tAF=0.99\n"
               "chrM\t410\t.\tA\tG\t.\tPASS\tAF=0.20\n"
               "chrM\t600\t.\tC\tT\t.\tPASS\tAF=0.50\n"
               "chrM\t700\t.\tG\tA\t.\tPASS\tAF=0.30\n")
    # A gVCF intermediate full of <NON_REF> + an off-target call must NOT inflate.
    _write_key(run_dir, "M117-bl.g.vcf.gz",
               "chrM\t150\t.\tT\t<NON_REF>\t.\t.\t.\n"
               "chrM\t9999\t.\tA\tT\t.\tPASS\tAF=0.99\n")

    plug = Nekrutenko()
    out = plug.collect(RunSpec(Arm.ECAA_WORKFLOW, run_dir, "ecaa_package", "x"), run_dir)
    task = Task(task_id="mtdna", prompt="", inputs={}, rubric=None,
                answer_key=key_dir, meta={})
    score = plug.score(task, Arm.ECAA_WORKFLOW, out, trial=0)
    assert abs(score.jaccard - 1.0) < 1e-9
    assert abs(score.overall - 100.0) < 1e-9
    assert score.dimensions == {}


def test_score_partial_overlap(tmp_path):
    """Half the answer-key calls present -> jaccard reflects union fraction."""
    key_dir = tmp_path / "ground_truth"; key_dir.mkdir()
    _write_key(key_dir, "M117-bl.vcf.gz", "chrM\t150\t.\tT\tC\t.\tPASS\tAF=0.99\n")
    _write_key(key_dir, "M117-ch.vcf.gz", "chrM\t410\t.\tA\tG\t.\tPASS\tAF=0.20\n")

    run_dir = tmp_path / "run"; run_dir.mkdir()
    # obs has 150 (match) + 999 (extra). union {150,410,999}=3, matched {150}=1.
    _write_key(run_dir, "snvs.raw.vcf.gz",
               "chrM\t150\t.\tT\tC\t.\tPASS\tAF=0.99\n"
               "chrM\t999\t.\tG\tT\t.\tPASS\tAF=0.50\n")
    plug = Nekrutenko()
    out = plug.collect(RunSpec(Arm.ECAA_WORKFLOW, run_dir, "ecaa_package", "x"), run_dir)
    task = Task(task_id="mtdna", prompt="", inputs={}, rubric=None,
                answer_key=key_dir, meta={})
    score = plug.score(task, Arm.ECAA_WORKFLOW, out, trial=0)
    assert abs(score.jaccard - (1 / 3)) < 1e-9


def test_score_preserves_error_cells_passthrough(tmp_path):
    """error_cells set on the Output's artifacts flows through unchanged."""
    key_dir = tmp_path / "ground_truth"; key_dir.mkdir()
    _write_key(key_dir, "M117-bl.vcf.gz", "chrM\t150\t.\tT\tC\t.\tPASS\tAF=0.99\n")
    cells = [{"pattern": "slow_tool", "tool": "bwa", "seed": 42,
              "handle": "recover", "recover": True, "diagnose": False}]
    out = Output(trace_md="", answer_txt="",
                 artifacts={"vcf_dir": tmp_path, "vcfs": {}, "error_cells": cells},
                 exit_ok=True, wall_secs=0.0)
    task = Task(task_id="mtdna", prompt="", inputs={}, rubric=None,
                answer_key=key_dir, meta={})
    score = Nekrutenko().score(task, Arm.ECAA_WORKFLOW, out, trial=0)
    assert score.error_cells == cells
