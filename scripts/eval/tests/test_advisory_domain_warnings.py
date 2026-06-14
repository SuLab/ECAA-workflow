"""Advisory / warn-only domain-correctness diagnostics surfacing.

When the harness runs with ``ECAA_HARNESS_CONTRACT_ADVISORY`` on, a failed
*required* validation-contract assertion is recorded in the ECAA package's
``runtime/validation-warnings.jsonl`` sidecar instead of blocking the task.
These tests cover the eval-side surfacing (BiomniBench plugin reader +
scorecard rollup/render). Pure visibility; no scoring math changes.
"""
import json

from scripts.eval.benchmark import Score, Scorecard
from scripts.eval.plugins.biomnibench import _collect_advisory_domain_warnings
from scripts.eval.services.scorecard import (
    _aggregate_advisory_domain_warnings,
    _markdown,
)


def _write_sidecar(run_dir, records):
    rt = run_dir / "runtime"
    rt.mkdir(parents=True, exist_ok=True)
    (rt / "validation-warnings.jsonl").write_text(
        "".join(json.dumps(r) + "\n" for r in records)
    )


def test_collect_reader_absent_sidecar_is_none(tmp_path):
    assert _collect_advisory_domain_warnings(tmp_path) is None


def test_collect_reader_summarizes_count_and_unique_sorted_ids(tmp_path):
    _write_sidecar(
        tmp_path,
        [
            {
                "task_id": "variant_calling",
                "assertion_id": "variant_calling.het_tail_band_nonempty",
                "severity": "required",
                "reason": "required assertion(s) unsatisfied: ...",
            },
            # Duplicate assertion id (same check seen twice) -> deduped in ids,
            # still counted in the raw count.
            {
                "task_id": "variant_calling",
                "assertion_id": "variant_calling.het_tail_band_nonempty",
                "severity": "required",
                "reason": "required assertion(s) unsatisfied: ...",
            },
            {
                "task_id": "differential_expression",
                "assertion_id": "differential_expression.design_recorded",
                "severity": "required",
                "reason": "required assertion(s) unsatisfied: ...",
            },
            "   ",  # non-object JSON line (a bare string) -> skipped
        ],
    )
    summary = _collect_advisory_domain_warnings(tmp_path)
    assert summary == {
        "count": 3,
        "assertion_ids": [
            "differential_expression.design_recorded",
            "variant_calling.het_tail_band_nonempty",
        ],
    }


def test_collect_reader_skips_malformed_lines(tmp_path):
    rt = tmp_path / "runtime"
    rt.mkdir(parents=True)
    (rt / "validation-warnings.jsonl").write_text(
        "{not json}\n"
        + json.dumps(
            {
                "task_id": "qc",
                "assertion_id": "qc.manifest_present",
                "severity": "required",
                "reason": "x",
            }
        )
        + "\n"
    )
    summary = _collect_advisory_domain_warnings(tmp_path)
    assert summary == {"count": 1, "assertion_ids": ["qc.manifest_present"]}


def _card_with_advisory():
    rows = [
        Score(
            "da-1", "ecaa", 0, 70.0, {}, None, None, "gemini",
            extra={
                "advisory_domain_warnings": {
                    "count": 2,
                    "assertion_ids": [
                        "variant_calling.het_tail_band_nonempty",
                        "qc.manifest_present",
                    ],
                }
            },
        ),
        # Bare arm has no advisory sidecar (no ECAA harness) — no key.
        Score("da-1", "claude-direct", 0, 60.0, {}, None, None, "gemini", extra={}),
    ]
    return Scorecard("biomnibench", rows, meta={})


def test_aggregate_rolls_up_per_arm_and_unions_ids():
    card = _card_with_advisory()
    agg = _aggregate_advisory_domain_warnings(card)
    assert set(agg) == {"ecaa"}
    assert agg["ecaa"]["warning_count"] == 2
    # Unioned + sorted across rows.
    assert agg["ecaa"]["assertion_ids"] == [
        "qc.manifest_present",
        "variant_calling.het_tail_band_nonempty",
    ]


def test_aggregate_empty_when_no_advisory_rows():
    rows = [Score("da-1", "ecaa", 0, 70.0, {}, None, None, "gemini", extra={})]
    assert _aggregate_advisory_domain_warnings(Scorecard("b", rows)) == {}


def test_markdown_renders_advisory_note_when_present():
    md = _markdown(_card_with_advisory())
    assert "Advisory domain-correctness warnings (NON-blocking diagnostics)" in md
    assert "ecaa: 2 advisory warning(s)" in md
    assert "variant_calling.het_tail_band_nonempty" in md


def test_markdown_omits_advisory_section_when_absent():
    rows = [Score("da-1", "ecaa", 0, 70.0, {}, None, None, "gemini", extra={})]
    md = _markdown(Scorecard("biomnibench", rows, meta={}))
    assert "Advisory domain-correctness warnings" not in md
