"""Tests for the declared entity-label ↔ accession annotation artifact.

The obligation that checks a citation is attached to the right entity needs a
table carrying BOTH roles. Before this artifact existed, the only satisfying
table in a real run was an agent-invented filename under `intermediates/` —
which the deposit export drops as reproducible bloat, so the evidence was
present in the working package and absent from every deposit. These tests pin
the artifact's shape, its skip rules, and its byte-determinism.
"""

from __future__ import annotations

import csv
from pathlib import Path

import pytest

from lib.literature.matrix import (
    SYMBOL_MAP_COLUMNS,
    SYMBOL_MAP_RELPATH,
    MatrixError,
    symbol_map_pairs,
    write_symbol_map,
)

HEADER_LINE = "symbol\tensembl_gene_id\n"


def _table(path: Path, header: list, rows: list) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="") as fh:
        writer = csv.writer(fh, delimiter="\t", lineterminator="\n")
        writer.writerow(header)
        writer.writerows(rows)
    return path


def _emit(tmp_path: Path, header: list, rows: list, **kwargs) -> Path:
    """Build a table, derive its map, write it, and return the written path."""
    source = _table(tmp_path / "results.tsv", header, rows)
    out = tmp_path / "out" / SYMBOL_MAP_RELPATH
    return write_symbol_map(symbol_map_pairs(source, **kwargs), out)


# --- both roles present ---------------------------------------------------


def test_both_roles_yield_a_two_column_tsv_sorted_by_accession(tmp_path: Path) -> None:
    written = _emit(
        tmp_path,
        ["gene", "symbol", "log2FoldChange", "padj"],
        [
            ["ENSGZZZ", "LATE", "1.0", "0.01"],
            ["ENSGAAA", "EARLY", "-1.0", "0.02"],
            ["ENSGMMM", "MIDDLE", "0.5", "0.03"],
        ],
    )
    assert written.read_text(encoding="utf-8") == (
        HEADER_LINE + "EARLY\tENSGAAA\nMIDDLE\tENSGMMM\nLATE\tENSGZZZ\n"
    )


def test_header_is_the_validator_convention_not_the_source_column_names(tmp_path: Path) -> None:
    """The header is fixed; the SOURCE columns are resolved by role, so a
    region-keyed table maps its own identifiers with no entity-specific code."""
    written = _emit(
        tmp_path,
        ["region_id", "entity", "logFC"],
        [["chr1:100-200", "PROMOTER_A", "0.4"]],
    )
    lines = written.read_text(encoding="utf-8").splitlines()
    assert lines[0].split("\t") == list(SYMBOL_MAP_COLUMNS)
    assert lines[1] == "PROMOTER_A\tchr1:100-200"


def test_one_row_per_distinct_mapping(tmp_path: Path) -> None:
    written = _emit(
        tmp_path,
        ["gene_id", "gene_symbol"],
        [
            ["ENSGAAA", "ONE"],
            ["ENSGAAA", "ONE"],
            ["ENSGAAA", "ONE_ALIAS"],
            ["ENSGBBB", "ONE"],
        ],
    )
    assert written.read_text(encoding="utf-8") == (
        HEADER_LINE + "ONE\tENSGAAA\nONE_ALIAS\tENSGAAA\nONE\tENSGBBB\n"
    )


def test_declared_columns_override_the_role_lists(tmp_path: Path) -> None:
    written = _emit(
        tmp_path,
        ["gene", "symbol", "hgnc_symbol"],
        [["ENSGAAA", "CANDIDATE_WINNER", "DECLARED_WINNER"]],
        symbol_column="hgnc_symbol",
    )
    assert written.read_text(encoding="utf-8") == HEADER_LINE + "DECLARED_WINNER\tENSGAAA\n"


def test_declared_but_absent_column_raises_rather_than_falling_back(tmp_path: Path) -> None:
    source = _table(tmp_path / "results.tsv", ["gene", "symbol"], [["ENSGAAA", "ONE"]])
    with pytest.raises(MatrixError, match="not in result table header"):
        symbol_map_pairs(source, symbol_column="nope")


# --- one role only → honest empty map ------------------------------------


def test_symbol_only_input_writes_a_header_only_file(tmp_path: Path) -> None:
    """No accession-role column at all: the file is written with just its
    header. An honest empty map is not the same as a missing artifact — the
    latter would read as "the task never ran the library"."""
    written = _emit(tmp_path, ["symbol", "log2FoldChange", "padj"], [["ONE", "1.0", "0.01"]])
    assert written.read_text(encoding="utf-8") == HEADER_LINE


def test_accession_only_input_writes_a_header_only_file(tmp_path: Path) -> None:
    """The production-reachable half of the same case: a result table with an
    identifier but no entity label carries no mapping to record."""
    written = _emit(tmp_path, ["gene", "log2FoldChange", "padj"], [["ENSGAAA", "1.0", "0.01"]])
    assert written.read_text(encoding="utf-8") == HEADER_LINE


def test_one_column_serving_both_roles_records_no_mapping(tmp_path: Path) -> None:
    source = _table(tmp_path / "results.tsv", ["gene", "logFC"], [["ONE", "1.0"]])
    assert symbol_map_pairs(source, id_column="gene", symbol_column="gene") == []


def test_header_only_source_table_yields_no_pairs(tmp_path: Path) -> None:
    source = _table(tmp_path / "results.tsv", ["gene", "symbol"], [])
    assert symbol_map_pairs(source) == []


# --- skip rules -----------------------------------------------------------


@pytest.mark.parametrize("marker", ["", " ", "NA", "na", "N/A", "NaN", "None", "null", "-", "."])
def test_missing_or_na_cells_are_skipped_on_either_side(tmp_path: Path, marker: str) -> None:
    written = _emit(
        tmp_path,
        ["gene", "symbol"],
        [
            ["ENSGAAA", marker],
            [marker, "ORPHAN"],
            ["ENSGBBB", "KEPT"],
        ],
    )
    assert written.read_text(encoding="utf-8") == HEADER_LINE + "KEPT\tENSGBBB\n"


def test_values_are_stripped_before_comparison(tmp_path: Path) -> None:
    written = _emit(tmp_path, ["gene", "symbol"], [["  ENSGAAA ", " ONE  "]])
    assert written.read_text(encoding="utf-8") == HEADER_LINE + "ONE\tENSGAAA\n"


def test_identity_pairs_record_no_mapping(tmp_path: Path) -> None:
    """A label equal to its own accession carries no mapping information, and
    emitting it would poison the column-role resolution of any reader that
    decides the label column by its content."""
    written = _emit(tmp_path, ["gene", "symbol"], [["ENSGAAA", "ENSGAAA"], ["ENSGBBB", "KEPT"]])
    assert written.read_text(encoding="utf-8") == HEADER_LINE + "KEPT\tENSGBBB\n"


def test_every_row_is_mapped_not_just_the_significant_set(tmp_path: Path) -> None:
    """The mapping is a property of the input annotation, not of a threshold."""
    written = _emit(
        tmp_path,
        ["gene", "symbol", "padj"],
        [["ENSGAAA", "SIGNIFICANT", "1e-9"], ["ENSGBBB", "NOT_SIGNIFICANT", "0.9"]],
    )
    assert written.read_text(encoding="utf-8") == (
        HEADER_LINE + "SIGNIFICANT\tENSGAAA\nNOT_SIGNIFICANT\tENSGBBB\n"
    )


# --- determinism ----------------------------------------------------------


def test_same_input_twice_is_byte_identical(tmp_path: Path) -> None:
    rows = [["ENSGBBB", "TWO"], ["ENSGAAA", "ONE"], ["ENSGCCC", "THREE"]]
    first = _emit(tmp_path / "a", ["gene", "symbol"], rows).read_bytes()
    second = _emit(tmp_path / "b", ["gene", "symbol"], rows).read_bytes()
    assert first == second


def test_row_order_of_the_source_does_not_change_the_output(tmp_path: Path) -> None:
    rows = [["ENSGBBB", "TWO"], ["ENSGAAA", "ONE"], ["ENSGCCC", "THREE"]]
    ordered = _emit(tmp_path / "a", ["gene", "symbol"], sorted(rows)).read_bytes()
    shuffled = _emit(tmp_path / "b", ["gene", "symbol"], list(reversed(rows))).read_bytes()
    assert ordered == shuffled


# --- path contract --------------------------------------------------------


def test_declared_path_is_not_under_a_tier_e_directory() -> None:
    """`intermediates/`, `cache/` and `__pycache__/` components are dropped by
    the deposit export, so the obligation's evidence must not live under one."""
    parts = Path(SYMBOL_MAP_RELPATH).parts
    assert parts[0] == "annotation"
    assert not ({"intermediates", "cache", "__pycache__", "view_data"} & set(parts))


def test_write_creates_the_parent_directory(tmp_path: Path) -> None:
    out = tmp_path / "task" / SYMBOL_MAP_RELPATH
    assert not out.parent.exists()
    write_symbol_map([("ONE", "ENSGAAA")], out)
    assert out.is_file()
