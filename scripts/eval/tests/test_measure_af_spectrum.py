"""Unit tests for the byte-pinned AF-spectrum measurement script.

The script is shipped into emitted packages' lib/ and run verbatim in the
bio-min container. These tests exercise its band-counting on hand-built
VCFs (no bcftools needed: we test the AF-list -> metrics core directly).
"""
import importlib.util
import json
from pathlib import Path

_SCRIPT = Path(__file__).resolve().parents[2] / "measure_af_spectrum.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("measure_af_spectrum", _SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_band_edges_are_operator_authored_constants():
    mod = _load_module()
    assert mod.NOISE_FLOOR == 0.01
    assert mod.HOMOPLASMY_CUTOFF == 0.5


def test_metrics_counts_low_af_band_and_sub_noise_floor():
    mod = _load_module()
    # AF list: 0.0016 (sub-noise), 0.04 (the dropped het, in band), 0.95 (homoplasmic)
    m = mod.compute_metrics([0.0016, 0.04, 0.95], n_samples=3)
    assert m["variant_count"] == 3
    assert m["n_samples"] == 3
    assert m["low_af_band_count"] == 1   # only 0.04 is in [0.01, 0.5)
    assert m["sub_noise_floor_count"] == 1  # 0.0016 < 0.01
    assert m["af_values"] == [0.0016, 0.04, 0.95]


def test_metrics_empty_band_when_het_dropped():
    mod = _load_module()
    # Het (0.04) dropped: only homoplasmic calls survive.
    m = mod.compute_metrics([0.95, 0.99], n_samples=3)
    assert m["low_af_band_count"] == 0
    assert m["sub_noise_floor_count"] == 0


def test_result_json_is_sorted_keys(tmp_path):
    mod = _load_module()
    out = tmp_path / "result.json"
    mod.write_result(mod.compute_metrics([0.04, 0.95], n_samples=2), out)
    text = out.read_text()
    parsed = json.loads(text)
    assert list(parsed.keys()) == sorted(parsed.keys()), "result.json keys must be sorted (determinism)"
