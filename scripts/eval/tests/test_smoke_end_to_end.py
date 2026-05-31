# scripts/eval/tests/test_smoke_end_to_end.py
"""Exercises the Nekrutenko score+report path end-to-end with stub artifacts,
no network, no live agent. Proves the plugin -> scorer -> scorecard wiring."""
from pathlib import Path
from scripts.eval.benchmark import Arm, Task, RunSpec
from scripts.eval.plugins.nekrutenko import Nekrutenko
from scripts.eval.services.scorecard import write_scorecard

VCF = "##fileformat=VCFv4.2\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\nchrM\t150\t.\tT\tC\t.\tPASS\tAF=0.99\n"

def test_end_to_end_nekrutenko(tmp_path):
    key = tmp_path / "ground_truth"; key.mkdir()
    (key / "s1.vcf").write_text(VCF)
    run = tmp_path / "run"; run.mkdir()
    (run / "s1.vcf").write_text(VCF)  # perfect match -> jaccard 1.0
    plug = Nekrutenko()
    task = Task("mtdna", "call variants", {}, None, key, {})
    spec = RunSpec(Arm.ECAA_WORKFLOW, run, "ecaa_package", "call variants")
    out = plug.collect(spec, run)
    score = plug.score(task, Arm.ECAA_WORKFLOW, out, 0)
    assert score.jaccard == 1.0 and score.overall == 100.0
    card = plug.report([score])
    out_dir = write_scorecard(card, tmp_path / "card")
    assert (out_dir / "scorecard.json").exists()
