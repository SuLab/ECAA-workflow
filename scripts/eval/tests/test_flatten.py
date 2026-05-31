# scripts/eval/tests/test_flatten.py
import json
from pathlib import Path
from scripts.eval.scoring.flatten import flatten_outputs

def _pkg(tmp_path):
    wf = {"tasks": [
        {"id": "load", "stage": "data_acquisition", "depends_on": []},
        {"id": "de", "stage": "differential_expression", "depends_on": ["load"]},
        {"id": "report", "stage": "final_reporting", "depends_on": ["de"]},
    ]}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    out = tmp_path / "runtime" / "outputs"
    for tid, txt in [("load","loaded 4 samples"), ("de","2018 sig genes"),
                     ("report","Treatment reduces recovery time.")]:
        d = out / tid; d.mkdir(parents=True)
        (d / "report.md").write_text(f"# {tid}\n{txt}\n")
    return tmp_path

def test_flatten_orders_and_picks_terminal(tmp_path):
    pkg = _pkg(tmp_path)
    trace, answer = flatten_outputs(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert trace.index("load") < trace.index("de") < trace.index("report")
    assert "Treatment reduces recovery time." in answer
