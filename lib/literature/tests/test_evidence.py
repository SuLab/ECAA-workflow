"""Evidence-snapshot tests: manifest resolution, integrity, quoting."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import pytest

from lib.literature.evidence import (
    EvidenceError,
    entity_pattern,
    load_evidence,
    mentions,
    reference_text,
    sentences,
    verify_quote,
)

SNIPPET = (
    "Asthma is a chronic inflammatory disease.\n"
    "Dexamethasone induced DUSP1 mRNA in airway smooth muscle cells.\n"
    "CD38 activity was measured separately."
)


def _write_snapshot(base: Path, body: str) -> str:
    digest = hashlib.sha256(body.encode("utf-8")).hexdigest()
    snapshots = base / "snapshots"
    snapshots.mkdir(parents=True, exist_ok=True)
    (snapshots / digest).write_text(body, encoding="utf-8")
    return digest


def _manifest(base: Path, entries: list) -> Path:
    base.mkdir(parents=True, exist_ok=True)
    path = base / "manifest.json"
    path.write_text(json.dumps({"schema_version": 2, "entries": entries}), encoding="utf-8")
    return path


def _fixture(tmp_path: Path, body: str = SNIPPET, pmid: str = "26825339") -> Path:
    base = tmp_path / "evidence"
    digest = _write_snapshot(base, body)
    return _manifest(
        base,
        [
            {
                "pmid": pmid,
                "source_ref_kind": "pmid",
                "source_ref": pmid,
                "source_kind": "abstract_only",
                "path": f"snapshots/{digest}",
                "sha256_binary": digest,
                "retrieval_ts": "2026-07-25T20:20:31Z",
                "redistributable": True,
                "license": "abstract_fair_use",
            }
        ],
    )


# --- reference text -------------------------------------------------------


def test_reference_text_collapses_line_wrapping() -> None:
    assert reference_text("a\n  b\t\tc \n") == "a b c"


def test_sentence_offsets_round_trip() -> None:
    text = reference_text(SNIPPET)
    for offset, sentence in sentences(text):
        assert text[offset : offset + len(sentence)] == sentence


# --- manifest resolution --------------------------------------------------


def test_load_evidence_resolves_snapshot_and_text(tmp_path: Path) -> None:
    manifest = _fixture(tmp_path)
    evidence = load_evidence(manifest)
    entry = evidence["26825339"]
    assert entry.source_hash.startswith("sha256:")
    assert "DUSP1" in entry.text
    assert "\n" not in entry.text


def test_missing_snapshot_is_skipped_not_fatal(tmp_path: Path) -> None:
    """A manifest may record a source whose bytes were not redistributable.
    It is simply not citable."""
    base = tmp_path / "evidence"
    manifest = _manifest(
        base,
        [
            {
                "pmid": "11111111",
                "path": "snapshots/deadbeef",
                "sha256_binary": "deadbeef",
                "source_kind": "abstract_only",
                "retrieval_ts": "",
                "redistributable": False,
                "license": "n/a",
            }
        ],
    )
    assert load_evidence(manifest) == {}


def test_substituted_snapshot_raises(tmp_path: Path) -> None:
    """Bytes that do not match the recorded digest are corruption or
    substitution, never an absence — citing them would be silent."""
    manifest = _fixture(tmp_path)
    entries = json.loads(manifest.read_text())["entries"]
    snapshot = manifest.parent / entries[0]["path"]
    snapshot.write_text("entirely different text", encoding="utf-8")
    with pytest.raises(EvidenceError, match="substituted"):
        load_evidence(manifest)


def test_hash_verification_can_be_disabled_for_diagnostics(tmp_path: Path) -> None:
    manifest = _fixture(tmp_path)
    entries = json.loads(manifest.read_text())["entries"]
    (manifest.parent / entries[0]["path"]).write_text("different", encoding="utf-8")
    assert load_evidence(manifest, verify_hashes=False)["26825339"].text == "different"


def test_duplicate_pmid_keeps_the_first_entry(tmp_path: Path) -> None:
    base = tmp_path / "evidence"
    first = _write_snapshot(base, "first body")
    second = _write_snapshot(base, "second body")
    manifest = _manifest(
        base,
        [
            {"pmid": "22222222", "path": f"snapshots/{first}", "sha256_binary": first,
             "source_kind": "abstract_only", "retrieval_ts": "", "redistributable": True,
             "license": "x"},
            {"pmid": "22222222", "path": f"snapshots/{second}", "sha256_binary": second,
             "source_kind": "abstract_only", "retrieval_ts": "", "redistributable": True,
             "license": "x"},
        ],
    )
    assert load_evidence(manifest)["22222222"].text == "first body"


def test_unreadable_manifest_raises(tmp_path: Path) -> None:
    with pytest.raises(EvidenceError):
        load_evidence(tmp_path / "nope.json")


# --- entity matching ------------------------------------------------------


def test_uppercase_symbols_match_case_sensitively() -> None:
    """`CAT`, `SET`, and `MAX` are gene symbols AND ordinary words. A
    case-insensitive match would attach citations to the wrong entity."""
    pattern = entity_pattern("CAT")
    assert pattern.search("Expression of CAT was measured.")
    assert not pattern.search("The cat sat on the mat.")


def test_mixed_case_identifiers_match_case_insensitively() -> None:
    pattern = entity_pattern("chr1:100:A:T")
    assert pattern.search("The variant chr1:100:A:T was called.")


def test_symbol_does_not_match_inside_a_longer_token() -> None:
    pattern = entity_pattern("DUSP1")
    assert not pattern.search("DUSP10 is a related phosphatase.")
    assert pattern.search("MKP-1, DUSP1) has emerged")


def test_single_character_symbol_is_rejected() -> None:
    assert entity_pattern("A") is None


def test_mentions_returns_only_entity_bearing_sentences(tmp_path: Path) -> None:
    entry = load_evidence(_fixture(tmp_path))["26825339"]
    hits = mentions(entry, "DUSP1")
    assert len(hits) == 1
    assert "Dexamethasone induced DUSP1" in hits[0][1]
    assert mentions(entry, "TP53") == []


# --- quote verification ---------------------------------------------------


def test_verify_quote_accepts_a_verbatim_substring(tmp_path: Path) -> None:
    entry = load_evidence(_fixture(tmp_path))["26825339"]
    offset, sentence = mentions(entry, "DUSP1")[0]
    assert verify_quote(entry, sentence, offset)


def test_verify_quote_rejects_a_shifted_offset(tmp_path: Path) -> None:
    entry = load_evidence(_fixture(tmp_path))["26825339"]
    offset, sentence = mentions(entry, "DUSP1")[0]
    assert not verify_quote(entry, sentence, offset + 1)


def test_verify_quote_rejects_text_absent_from_the_snapshot(tmp_path: Path) -> None:
    entry = load_evidence(_fixture(tmp_path))["26825339"]
    assert not verify_quote(entry, "DUSP1 was repressed by dexamethasone.", 0)
    assert not verify_quote(entry, "", 0)
