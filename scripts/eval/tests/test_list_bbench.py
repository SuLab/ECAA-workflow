"""Offline coverage for the eval-list-bbench catalog printer. Pure filesystem
scan; no network."""
from scripts.eval import list_bbench


def test_no_cache_degrades_gracefully(monkeypatch, tmp_path, capsys):
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path / "missing"))
    rc = list_bbench.main([])
    out = capsys.readouterr().out
    assert rc == 0
    assert "no cached BiomniBench-DA dataset" in out


def test_lists_task_dirs_with_meta_and_size(monkeypatch, tmp_path, capsys):
    cache = tmp_path / "cache"
    task = cache / "phylobio__BiomniBench-DA@abc123def456" / "da-8-1"
    (task / "environment" / "data").mkdir(parents=True)
    (task / "environment" / "data" / "matrix.csv").write_text("a,b\n1,2\n")
    (task / "task.toml").write_text(
        'category = "metabolic"\ndifficulty = "easy"\n')
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(cache))

    rc = list_bbench.main([])
    out = capsys.readouterr().out
    assert rc == 0
    assert "da-8-1" in out
    assert "metabolic" in out
    assert "easy" in out
    # The header is printed.
    assert "category" in out and "difficulty" in out


def test_missing_task_toml_degrades_to_question_marks(monkeypatch, tmp_path, capsys):
    cache = tmp_path / "cache"
    task = cache / "phylobio__BiomniBench-DA@abc" / "da-1-1"
    task.mkdir(parents=True)
    # No task.toml, no environment/data.
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(cache))

    rc = list_bbench.main([])
    out = capsys.readouterr().out
    assert rc == 0
    # Row present with '?' meta and '-' data size.
    line = next(l for l in out.splitlines() if l.startswith("da-1-1"))
    assert "?" in line
    assert line.rstrip().endswith("-")


def test_nested_task_table_meta(monkeypatch, tmp_path, capsys):
    cache = tmp_path / "cache"
    task = cache / "phylobio__BiomniBench-DA@x" / "da-5-1"
    task.mkdir(parents=True)
    (task / "task.toml").write_text(
        '[task]\ncategory = "oncology"\ndifficulty = "medium"\n')
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(cache))

    rc = list_bbench.main([])
    out = capsys.readouterr().out
    assert rc == 0
    assert "oncology" in out and "medium" in out
