# scripts/eval/tests/test_eval_runner.py
from pathlib import Path
from scripts.eval import eval_runner  # see Step 3: module named eval_runner.py

def test_registry_has_both():
    assert set(eval_runner.PLUGINS) == {"biomnibench", "nekrutenko"}

def test_skip_without_live_flag(monkeypatch, capsys):
    monkeypatch.delenv("ECAA_EVAL_LIVE", raising=False)
    rc = eval_runner.main(["biomnibench", "--smoke"])
    assert rc == 0
    assert "SKIP" in capsys.readouterr().out

def test_stage_inputs_copies_into_package(tmp_path):
    """_stage_inputs creates pkg/inputs/ and copies each source file into it."""
    # Create fake source input files in a separate directory.
    src_dir = tmp_path / "sources"
    src_dir.mkdir()
    file_a = src_dir / "sample_A.fastq"
    file_b = src_dir / "sample_B.fastq"
    file_a.write_text("@read1\nACGT\n+\nIIII\n")
    file_b.write_text("@read2\nTGCA\n+\nIIII\n")

    pkg_dir = tmp_path / "pkg"
    pkg_dir.mkdir()

    inputs = {"sample_A": file_a, "sample_B": file_b}
    eval_runner._stage_inputs(pkg_dir, inputs)

    inputs_dir = pkg_dir / "inputs"
    assert inputs_dir.is_dir(), "inputs/ subdirectory should be created"
    assert (inputs_dir / "sample_A.fastq").exists(), "sample_A.fastq should be copied"
    assert (inputs_dir / "sample_B.fastq").exists(), "sample_B.fastq should be copied"
    assert (inputs_dir / "sample_A.fastq").read_text() == file_a.read_text()
    assert (inputs_dir / "sample_B.fastq").read_text() == file_b.read_text()

def test_stage_inputs_skips_missing_source(tmp_path):
    """_stage_inputs silently skips input files whose source path does not exist."""
    pkg_dir = tmp_path / "pkg"
    pkg_dir.mkdir()
    missing = tmp_path / "nonexistent.fastq"  # deliberately not created
    existing = tmp_path / "real.fastq"
    existing.write_text("data")

    eval_runner._stage_inputs(pkg_dir, {"miss": missing, "real": existing})

    inputs_dir = pkg_dir / "inputs"
    assert not (inputs_dir / "nonexistent.fastq").exists(), "missing source should be skipped"
    assert (inputs_dir / "real.fastq").exists(), "existing source should be copied"
