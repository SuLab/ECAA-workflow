"""End-to-end tests for the contextualization pipeline.

The fixture mirrors the deposited himes run in miniature: a DE table, a
prior-claims matrix whose query axis names one gene, and three snapshots —
one that reports a direction for that gene, one that mentions it without a
direction, and one that never mentions it at all.
"""

from __future__ import annotations

import csv
import hashlib
import json
from pathlib import Path

import pytest

from lib.literature.contextualize import COUNT_DEFINITIONS, build_rows, contextualize
from lib.literature.direction import CONCORDANCE_FLAGS
from lib.literature.evidence import load_evidence
from lib.literature.matrix import (
    COLUMNS,
    SYMBOL_MAP_COLUMNS,
    SYMBOL_MAP_RELPATH,
    MatrixError,
    read_prior_claims,
    read_result_table,
    searched_entities,
)

DIRECTIONAL = (
    "Acting on the glucocorticoid receptor, glucocorticoids are widely used. "
    "Dexamethasone induced DUSP1 mRNA in airway smooth muscle cells."
)
MENTION_ONLY = (
    "Asthma affects millions worldwide. "
    "We identified differentially expressed genes including DUSP1 and KLF15."
)
UNRELATED = "The enzymatic activity of CD38 synthesizes cyclic ADP-ribose."

DE_ROWS = [
    # gene, symbol, log2FoldChange, padj
    ("ENSG00000120129", "DUSP1", "2.947850", "1e-40"),
    ("ENSG00000101347", "SAA1", "3.100000", "1e-30"),
    ("ENSG00000000003", "TSPAN6", "-1.200000", "0.9"),  # not significant
]


def _snapshot(base: Path, body: str) -> str:
    digest = hashlib.sha256(body.encode("utf-8")).hexdigest()
    (base / "snapshots").mkdir(parents=True, exist_ok=True)
    (base / "snapshots" / digest).write_text(body, encoding="utf-8")
    return digest


@pytest.fixture()
def workspace(tmp_path: Path) -> dict:
    evidence_dir = tmp_path / "review_prior_work" / "evidence"
    evidence_dir.mkdir(parents=True)
    bodies = {
        "25625944": DIRECTIONAL,
        "24926665": MENTION_ONLY,
        "18441094": UNRELATED,
    }
    entries = []
    for pmid, body in bodies.items():
        digest = _snapshot(evidence_dir, body)
        entries.append(
            {
                "pmid": pmid,
                "source_ref_kind": "pmid",
                "source_ref": pmid,
                "source_kind": "abstract_only",
                "path": f"snapshots/{digest}",
                "sha256_binary": digest,
                "sha256_extracted_text": digest,
                "extracted_text_normalization": "collapse_whitespace_lowercase_v1",
                "bytes": len(body),
                "retrieval_ts": "2026-07-25T20:20:31Z",
                "retrieval_query_id": "q001",
                "redistributable": True,
                "license": "abstract_fair_use",
            }
        )
    manifest = evidence_dir / "manifest.json"
    manifest.write_text(json.dumps({"schema_version": 2, "entries": entries}), encoding="utf-8")

    prior = tmp_path / "review_prior_work" / "prior_claims_matrix.csv"
    with prior.open("w", encoding="utf-8", newline="") as fh:
        w = csv.writer(fh, lineterminator="\n")
        w.writerow(["axis", "pmid", "source_hash", "evidence_quote"])
        for pmid in bodies:
            w.writerow(["dusp1_dexamethasone_asm", pmid, f"sha256:{pmid}", ""])

    results = tmp_path / "de_results.tsv"
    with results.open("w", encoding="utf-8", newline="") as fh:
        w = csv.writer(fh, delimiter="\t", lineterminator="\n")
        w.writerow(["gene", "symbol", "log2FoldChange", "padj"])
        for row in DE_ROWS:
            w.writerow(row)

    return {
        "results": results,
        "prior": prior,
        "manifest": manifest,
        "out": tmp_path / "ctx",
    }


def _run(workspace: dict, **kwargs) -> dict:
    params = dict(
        entity_kind="gene",
        symbol_column="symbol",
        effect_column="log2FoldChange",
        significance_column="padj",
        threshold=0.05,
    )
    params.update(kwargs)
    return contextualize(
        workspace["results"],
        workspace["prior"],
        workspace["manifest"],
        workspace["out"],
        **params,
    )


def _matrix(workspace: dict) -> list:
    with (workspace["out"] / "claims_evidence_matrix.csv").open(encoding="utf-8") as fh:
        return list(csv.DictReader(fh))


# --- result-table reading -------------------------------------------------


def test_threshold_filters_to_the_significant_set(workspace: dict) -> None:
    table = read_result_table(
        workspace["results"], symbol_column="symbol", significance_column="padj", threshold=0.05
    )
    assert [f.symbol for f in table.findings] == ["SAA1", "DUSP1"]  # sorted by finding_id
    assert table.n_rows == 3
    assert table.n_significant == 2


def test_all_rows_mode_keeps_the_insignificant_rows(workspace: dict) -> None:
    table = read_result_table(workspace["results"], symbol_column="symbol", significant_only=False)
    assert len(table.findings) == 3


def test_declared_but_absent_column_is_an_error_not_a_fallback(workspace: dict) -> None:
    """Ranging over candidates after the caller named a column would attach
    citations to a different measurement than the one requested."""
    with pytest.raises(MatrixError, match="not in result table header"):
        read_result_table(workspace["results"], effect_column="stat")


def test_missing_symbol_column_uses_the_identifier_as_the_entity(tmp_path: Path) -> None:
    """The library never maps ids to symbols itself — identifier resolution
    belongs to the run's pinned annotation."""
    path = tmp_path / "variants.tsv"
    path.write_text("variant_id\tbeta\tp_value\nchr1:100:A:T\t0.4\t0.001\n", encoding="utf-8")
    table = read_result_table(path, threshold=0.05)
    assert table.findings[0].symbol == "chr1:100:A:T"
    assert table.effect_column == "beta"


def test_variant_measurements_use_modality_neutral_matrix_fields(tmp_path: Path) -> None:
    path = tmp_path / "variants.tsv"
    path.write_text("variant_id\tbeta\tp_value\nchr1:100:A:T\t0.4\t0.001\n", encoding="utf-8")
    table = read_result_table(path, threshold=0.05)
    rows = build_rows(table, [], {}, entity_kind="variant")

    assert len(rows) == 1
    assert float(rows[0].analysis_effect) == 0.4
    assert float(rows[0].analysis_significance) == 0.001
    assert not hasattr(rows[0], "analysis_log2fc")


# --- searched-set boundary ------------------------------------------------


def test_query_axis_defines_the_searched_set(workspace: dict) -> None:
    prior = read_prior_claims(workspace["prior"])
    scope = searched_entities(prior, ["DUSP1", "SAA1"])
    assert set(scope) == {"DUSP1"}


def test_explicit_scope_overrides_axis_inference(workspace: dict) -> None:
    prior = read_prior_claims(workspace["prior"])
    scope = searched_entities(prior, ["DUSP1", "SAA1"], explicit=["SAA1"])
    assert set(scope) == {"SAA1"}


def test_no_recoverable_scope_means_everything_is_not_assessed(workspace: dict) -> None:
    """The library will not manufacture a searched set to produce a
    prettier matrix."""
    assert searched_entities([], ["DUSP1"]) == {}


def test_unsearched_entity_is_not_assessed_never_novel(workspace: dict) -> None:
    _run(workspace)
    rows = {r["entity"]: r for r in _matrix(workspace)}
    assert rows["SAA1"]["concordance_flag"] == "not_assessed"
    assert rows["SAA1"]["searched"] == "false"


def test_broad_query_retains_exact_entity_evidence(workspace: dict) -> None:
    """A study-level query can retrieve direct evidence for an entity even
    though the query axis does not spell out that entity's symbol."""
    prior_rows = read_prior_claims(workspace["prior"])
    for row in prior_rows:
        row["axis"] = "himes_2014_airway_study"
    with workspace["prior"].open("w", encoding="utf-8", newline="") as fh:
        writer = csv.DictWriter(
            fh,
            fieldnames=["axis", "pmid", "source_hash", "evidence_quote"],
            lineterminator="\n",
        )
        writer.writeheader()
        writer.writerows(prior_rows)

    summary = _run(workspace)
    dusp1 = [r for r in _matrix(workspace) if r["entity"] == "DUSP1"]
    assert {r["prior_pmid"] for r in dusp1} == {"24926665", "25625944"}
    assert any(r["concordance_flag"] == "same_direction" for r in dusp1)
    assert all(r["searched"] == "true" for r in dusp1)
    assert summary["n_entities_assessed"] == 1
    assert "DUSP1" in summary["entities_assessed"]


def test_searched_entity_with_no_naming_snapshot_is_no_prior_finding(workspace: dict) -> None:
    _run(workspace, explicit_searched=["SAA1"])
    rows = {r["entity"]: r for r in _matrix(workspace)}
    assert rows["SAA1"]["concordance_flag"] == "no_prior_finding"
    assert rows["SAA1"]["searched"] == "true"


def test_zero_result_axis_survives_in_retained_retrieval_scope(workspace: dict) -> None:
    """An attempted axis with no claim row is searched, not not_assessed."""
    scope_path = workspace["prior"].parent / "retrieval_scope.json"
    scope_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "axes": [
                    {
                        "axis": "dusp1_dexamethasone_asm",
                        "query": "DUSP1 dexamethasone airway smooth muscle",
                        "status": "completed",
                        "entries_written": 3,
                        "rows_written": 3,
                        "fallback_used": False,
                        "truncated_at_storage_cap": False,
                    },
                    {
                        "axis": "SAA1",
                        "query": "SAA1 dexamethasone airway smooth muscle",
                        "status": "completed",
                        "entries_written": 0,
                        "rows_written": 0,
                        "fallback_used": False,
                        "truncated_at_storage_cap": False,
                    },
                    {
                        "axis": "dusp1_dexamethasone_asm",
                        "query": "DUSP1 glucocorticoid response",
                        "status": "completed",
                        "entries_written": 0,
                        "rows_written": 0,
                        "fallback_used": False,
                        "truncated_at_storage_cap": False,
                    },
                ],
            }
        ),
        encoding="utf-8",
    )

    summary = _run(workspace)
    rows = {r["entity"]: r for r in _matrix(workspace)}
    assert rows["SAA1"]["concordance_flag"] == "no_prior_finding"
    assert rows["SAA1"]["searched"] == "true"
    assert summary["n_search_axes_total"] == 2
    assert summary["search_axes_total"] == ["SAA1", "dusp1_dexamethasone_asm"]
    assert summary["search_scope_source"] == "retrieval_scope"


# --- concordance ----------------------------------------------------------


def test_directional_snapshot_yields_same_direction(workspace: dict) -> None:
    _run(workspace)
    hit = [r for r in _matrix(workspace) if r["prior_pmid"] == "25625944"]
    assert len(hit) == 1
    assert hit[0]["analysis_direction"] == "up"
    assert hit[0]["prior_direction"] == "up"
    assert hit[0]["concordance_flag"] == "same_direction"


def test_opposite_sign_yields_opposite_direction(tmp_path: Path, workspace: dict) -> None:
    flipped = tmp_path / "flipped.tsv"
    with flipped.open("w", encoding="utf-8", newline="") as fh:
        w = csv.writer(fh, delimiter="\t", lineterminator="\n")
        w.writerow(["gene", "symbol", "log2FoldChange", "padj"])
        w.writerow(["ENSG00000120129", "DUSP1", "-2.5", "1e-40"])
    workspace["results"] = flipped
    _run(workspace)
    hit = [r for r in _matrix(workspace) if r["prior_pmid"] == "25625944"]
    assert hit[0]["concordance_flag"] == "opposite_direction"


def test_mention_without_direction_is_unverifiable(workspace: dict) -> None:
    _run(workspace)
    hit = [r for r in _matrix(workspace) if r["prior_pmid"] == "24926665"]
    assert hit[0]["concordance_flag"] == "unverifiable"
    assert hit[0]["prior_direction"] == ""


def test_snapshot_never_naming_the_entity_is_not_cited(workspace: dict) -> None:
    """The hand-written script attached a CD38 quote to DUSP1 as
    `unverifiable`. A snapshot that never names the entity is not evidence
    about it, so no row cites it."""
    _run(workspace)
    assert not [r for r in _matrix(workspace) if r["prior_pmid"] == "18441094"]


def test_every_flag_is_inside_the_closed_set(workspace: dict) -> None:
    _run(workspace)
    for row in _matrix(workspace):
        assert row["concordance_flag"] in CONCORDANCE_FLAGS


# --- emitted artifacts ----------------------------------------------------


def test_matrix_columns_and_verified_quotes(workspace: dict) -> None:
    _run(workspace)
    rows = _matrix(workspace)
    assert list(rows[0].keys()) == list(COLUMNS)
    evidence = load_evidence(workspace["manifest"])
    for row in rows:
        if not row["prior_pmid"]:
            assert row["source_hash"] == "none"
            continue
        entry = evidence[row["prior_pmid"]]
        offset = int(row["evidence_quote_offset"])
        quote = row["evidence_quote"]
        # Every emitted quote is a verbatim substring at its recorded offset.
        assert entry.text[offset : offset + len(quote)] == quote
        assert row["source_hash"] == entry.source_hash
        assert row["retrieval_ts"] == entry.retrieval_ts


def test_matrix_schema_matches_the_shared_emitter_contract() -> None:
    schema_path = (
        Path(__file__).resolve().parents[3]
        / "config"
        / "stage-atoms"
        / "schemas"
        / "claims_evidence_matrix.schema.json"
    )
    schema = json.loads(schema_path.read_text(encoding="utf-8"))
    assert set(schema["required"]) == set(COLUMNS)
    assert set(schema["properties"]) == set(COLUMNS)


def test_no_pmid_or_sha256_literal_is_embedded_in_the_library() -> None:
    """The whole point. The hand-written script this library replaces
    carried four PMIDs, four quotes, four direction labels, and four
    sha256 digests as literals; nothing here may."""
    import re

    import lib.literature as package

    pmid_shaped = re.compile(r"(?<![0-9])[1-9][0-9]{6,8}(?![0-9])")
    sha_shaped = re.compile(r"(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])")
    root = Path(package.__file__).parent
    offenders = []
    for path in sorted(root.glob("*.py")):
        source = path.read_text(encoding="utf-8")
        offenders += [f"{path.name}: PMID-shaped {m}" for m in pmid_shaped.findall(source)]
        offenders += [f"{path.name}: sha256-shaped {m}" for m in sha_shaped.findall(source)]
    assert not offenders, offenders


def test_evidence_manifest_covers_every_cited_pmid(workspace: dict) -> None:
    summary = _run(workspace)
    manifest = json.loads(
        (workspace["out"] / "evidence" / "manifest.json").read_text(encoding="utf-8")
    )
    recorded = {e["pmid"] for e in manifest["entries"]}
    assert recorded == set(summary["cited_pmids"])
    for entry in manifest["entries"]:
        assert (workspace["out"] / "evidence" / entry["path"]).is_file()


def test_declared_artifacts_are_all_written(workspace: dict) -> None:
    _run(workspace)
    for name in (
        "claims_evidence_matrix.csv",
        "literature_search_protocol.md",
        "citation_verification_report.md",
        "evidence/manifest.json",
        SYMBOL_MAP_RELPATH,
    ):
        assert (workspace["out"] / name).is_file()


def test_symbol_map_carries_every_input_mapping(workspace: dict) -> None:
    """The declared annotation artifact records the label↔accession mapping the
    input carried — including the row the significance threshold excluded, so
    the map is a property of the annotation and not of the threshold."""
    summary = _run(workspace)
    written = (workspace["out"] / SYMBOL_MAP_RELPATH).read_text(encoding="utf-8")
    header, *rows = written.splitlines()
    assert header.split("\t") == list(SYMBOL_MAP_COLUMNS)
    assert rows == [
        f"{symbol}\t{gene}" for gene, symbol, _effect, _padj in sorted(DE_ROWS)
    ]
    assert summary["symbol_map"] == {"path": SYMBOL_MAP_RELPATH, "n_rows": len(DE_ROWS)}


def test_report_states_when_contrast_grounding_was_not_applied(workspace: dict) -> None:
    _run(workspace)
    report = (workspace["out"] / "citation_verification_report.md").read_text(encoding="utf-8")
    assert "No contrast terms were supplied" in report

    _run(workspace, contrast_terms=["dexamethasone"])
    report = (workspace["out"] / "citation_verification_report.md").read_text(encoding="utf-8")
    assert "dexamethasone" in report
    assert "No contrast terms were supplied" not in report


def test_output_is_byte_identical_across_runs(workspace: dict, tmp_path: Path) -> None:
    _run(workspace)
    first = (workspace["out"] / "claims_evidence_matrix.csv").read_bytes()
    workspace["out"] = tmp_path / "ctx2"
    _run(workspace)
    assert (workspace["out"] / "claims_evidence_matrix.csv").read_bytes() == first


def test_summary_counts_match_the_matrix(workspace: dict) -> None:
    summary = _run(workspace)
    rows = _matrix(workspace)
    assert summary["n_rows"] == len(rows)
    tally: dict = {}
    for row in rows:
        tally[row["concordance_flag"]] = tally.get(row["concordance_flag"], 0) + 1
    assert summary["concordance_counts"] == tally


@pytest.mark.parametrize("marker", ["", "NA", "N/A", "NaN", "None", "null", "-", "."])
def test_missing_symbol_markers_fall_back_to_the_finding_id(
    workspace: dict, marker: str
) -> None:
    with workspace["results"].open("w", encoding="utf-8", newline="") as fh:
        writer = csv.writer(fh, delimiter="\t", lineterminator="\n")
        writer.writerow(["gene", "symbol", "log2FoldChange", "padj"])
        writer.writerow(["ENSG_UNMAPPED", marker, "1.0", "0.01"])

    table = read_result_table(
        workspace["results"],
        symbol_column="symbol",
        effect_column="log2FoldChange",
        significance_column="padj",
    )

    assert len(table.findings) == 1
    assert table.findings[0].symbol == "ENSG_UNMAPPED"


# --- self-describing assessment counts ------------------------------------
#
# The fixture is chosen so the entity count and the evidence-ROW count DIFFER:
# DUSP1 is the single searched entity and it cites TWO snapshots, so 1 entity
# yields 2 rows. That difference is the whole point — a deposited report read 9
# assessed evidence rows as "9 specific genes" when only 4 entities were ever
# searched, and contradicted itself with the correct 4 later in the same file.


def test_entity_and_evidence_row_counts_are_separate_numbers(workspace: dict) -> None:
    summary = _run(workspace)
    rows = _matrix(workspace)
    searched = [r for r in rows if r["searched"] == "true"]
    assert summary["n_entities_assessed"] == 1
    assert summary["n_evidence_rows_assessed"] == 2
    # The two are distinct keys with distinct values, so neither can be quoted as
    # the other. Both reconcile against the emitted CSV.
    assert summary["n_evidence_rows_assessed"] != summary["n_entities_assessed"]
    assert summary["n_evidence_rows_assessed"] == len(searched)
    assert summary["n_entities_assessed"] == len({r["entity"] for r in searched})
    assert summary["entities_assessed"] == ["DUSP1"]
    # Only the entity count may be described as a number of entities assessed.
    assert summary["n_evidence_rows_total"] == len(rows) == summary["n_rows"]


def test_not_assessed_entity_count_is_separate_from_the_assessed_one(
    workspace: dict,
) -> None:
    summary = _run(workspace)
    # SAA1 is significant but no query named it, so it is not_assessed — a
    # distinct bucket from the assessed entities, never novel.
    assert summary["n_entities_not_assessed"] == 1
    assert summary["n_entities_assessed"] == 1
    rows = _matrix(workspace)
    unsearched = {r["entity"] for r in rows if r["searched"] != "true"}
    assert unsearched == {"SAA1"}
    assert set(summary["entities_assessed"]).isdisjoint(unsearched)


def test_axis_total_is_separate_from_axes_naming_an_entity(workspace: dict) -> None:
    """A retrieval axis may name a method or a dataset rather than an entity, so
    the total axis count is NOT a count of entities searched. The deposited run
    recorded 7 axes of which only 4 named an entity."""
    prior_rows = read_prior_claims(workspace["prior"])
    # Two more axes that name no finding entity at all.
    for axis in ("deseq2_bulk_rnaseq_de", "dexamethasone_airway_smooth_muscle"):
        prior_rows.append(
            {"axis": axis, "pmid": "24926665", "source_hash": "", "evidence_quote": ""}
        )
    with workspace["prior"].open("w", encoding="utf-8", newline="") as fh:
        writer = csv.DictWriter(
            fh, fieldnames=["axis", "pmid", "source_hash", "evidence_quote"],
            lineterminator="\n",
        )
        writer.writeheader()
        writer.writerows(prior_rows)

    summary = _run(workspace)
    assert summary["n_search_axes_total"] == 3
    assert summary["n_search_axes_naming_an_assessed_entity"] == 1
    assert summary["search_axes_naming_an_assessed_entity"] == ["dusp1_dexamethasone_asm"]
    assert summary["search_scope_source"] == "query_axes"


#: The counts this change adds, spelled out so definition coverage is checked
#: against a literal set rather than against COUNT_DEFINITIONS itself. The
#: pre-existing `n_rows` / `n_findings` / `n_snapshots_available` are deliberately
#: absent: their meanings are unchanged and undocumented-here by design.
_NEW_COUNT_KEYS = {
    "n_entities_assessed",
    "n_entities_not_assessed",
    "n_evidence_rows_assessed",
    "n_evidence_rows_total",
    "n_search_axes_total",
    "n_search_axes_naming_an_assessed_entity",
}


def test_every_emitted_count_carries_its_own_definition(workspace: dict) -> None:
    summary = _run(workspace)
    definitions = summary["count_definitions"]
    assert definitions == COUNT_DEFINITIONS
    # Every new count is defined, and every definition names an emitted count —
    # so a narrative can never quote one of these numbers with no denominator.
    assert set(COUNT_DEFINITIONS) == _NEW_COUNT_KEYS, (
        set(COUNT_DEFINITIONS) ^ _NEW_COUNT_KEYS
    )
    for key in _NEW_COUNT_KEYS:
        assert key in summary, key
        assert isinstance(summary[key], int), key
        text = definitions[key]
        assert text and text[0].islower(), key
        # A definition that does not name what it counts is not a definition.
        assert ("entit" in text) or ("row" in text) or ("axes" in text), key


def test_explicit_scope_records_why_the_axis_count_is_zero(workspace: dict) -> None:
    """An explicit scope list means the retrieval step handed us its own searched
    set, so no axis named the entities. Recording the source stops a reader from
    reading that 0 as 'nothing was searched'."""
    summary = _run(workspace, explicit_searched=["DUSP1"])
    assert summary["search_scope_source"] == "explicit"
    assert summary["n_search_axes_naming_an_assessed_entity"] == 0
    assert summary["n_entities_assessed"] == 1
    assert summary["n_evidence_rows_assessed"] == 2


def test_assessment_counts_are_deterministic(workspace: dict, tmp_path: Path) -> None:
    first = _run(workspace)
    workspace["out"] = tmp_path / "ctx_again"
    second = _run(workspace)
    for key in (
        "n_entities_assessed",
        "n_entities_not_assessed",
        "n_evidence_rows_assessed",
        "n_evidence_rows_total",
        "n_search_axes_total",
        "n_search_axes_naming_an_assessed_entity",
        "entities_assessed",
        "search_axes_total",
        "search_axes_naming_an_assessed_entity",
        "search_scope_source",
        "count_definitions",
    ):
        assert first[key] == second[key], key


def test_pre_existing_summary_keys_keep_their_meanings(workspace: dict) -> None:
    """The new counts are additive: nothing an existing reader binds to moved."""
    summary = _run(workspace)
    rows = _matrix(workspace)
    assert summary["n_rows"] == len(rows)
    assert summary["n_findings"] == 2  # DUSP1 + SAA1 pass the threshold
    assert summary["n_snapshots_available"] == 3
    assert summary["columns"]["symbol"] == "symbol"
    assert summary["symbol_map"] == {"path": SYMBOL_MAP_RELPATH, "n_rows": len(DE_ROWS)}


# --- guardrails -----------------------------------------------------------


def test_unknown_entity_kind_is_rejected(workspace: dict) -> None:
    table = read_result_table(workspace["results"], symbol_column="symbol")
    with pytest.raises(MatrixError, match="entity_kind"):
        build_rows(table, [], {}, entity_kind="pathway")


def test_pmid_without_a_retrievable_snapshot_is_dropped(workspace: dict) -> None:
    """A PMID named by the prior-claims matrix but absent from the evidence
    manifest is an unsupported citation, so it never reaches the matrix."""
    prior = read_prior_claims(workspace["prior"])
    prior.append({"axis": "dusp1_dexamethasone_asm", "pmid": "99999999"})
    table = read_result_table(
        workspace["results"], symbol_column="symbol", significance_column="padj", threshold=0.05
    )
    rows = build_rows(table, prior, load_evidence(workspace["manifest"]))
    assert "99999999" not in {r.prior_pmid for r in rows}
