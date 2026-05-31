# scripts/eval/tests/test_eval_runner.py
from scripts.eval import eval_runner  # see Step 3: module named eval_runner.py

def test_registry_has_both():
    assert set(eval_runner.PLUGINS) == {"biomnibench", "nekrutenko"}

def test_skip_without_live_flag(monkeypatch, capsys):
    monkeypatch.delenv("ECAA_EVAL_LIVE", raising=False)
    rc = eval_runner.main(["biomnibench", "--smoke"])
    assert rc == 0
    assert "SKIP" in capsys.readouterr().out
