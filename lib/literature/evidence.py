"""Evidence-snapshot access: manifest parsing, integrity, quote extraction.

Every citation this library emits is anchored to a snapshot file that was
actually retrieved and stored. Nothing here ever accepts a PMID, a quote,
or a hash as a literal from calling code — a PMID is usable only if it
resolves to a manifest entry whose snapshot is on disk and whose bytes
hash to the digest the manifest recorded, and a quote is usable only if
it is a verbatim substring of that snapshot at a recorded offset.

Reference text
--------------
Quotes and offsets are computed against the snapshot's *reference text*:
the file decoded as UTF-8 with every run of whitespace (including
newlines) collapsed to a single space. Line wrapping is a storage
artifact of the fetch, not part of the sentence, so collapsing it is what
makes a quote quotable. Offsets index that reference text, and
`quote_at(offset)` round-trips.
"""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, List, Optional

_WHITESPACE = re.compile(r"\s+")

#: Sentence boundary: terminal punctuation followed by whitespace. Kept
#: deliberately simple — an abbreviation ("e.g. ") splits a sentence early,
#: which can only SHORTEN a quote. It can never fabricate text that is not
#: in the snapshot, because every quote is substring-verified afterwards.
_SENTENCE_SPLIT = re.compile(r"(?<=[.!?])\s+")


class EvidenceError(RuntimeError):
    """Raised when a citation cannot be anchored to retrieved evidence."""


@dataclass(frozen=True)
class EvidenceEntry:
    """One resolved snapshot: the manifest record plus its reference text."""

    pmid: str
    source_ref: str
    source_kind: str
    path: str
    sha256_binary: str
    retrieval_ts: str
    redistributable: bool
    license: str
    text: str

    @property
    def source_hash(self) -> str:
        """`sha256:<64 hex>` — the `source_hash` column's declared shape."""
        return f"sha256:{self.sha256_binary}"


def reference_text(raw: str) -> str:
    """Collapse all whitespace to single spaces and strip the ends."""
    return _WHITESPACE.sub(" ", raw).strip()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_manifest(manifest_path: Path) -> List[dict]:
    """Read a literature evidence manifest's `entries` list."""
    try:
        payload = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise EvidenceError(f"unreadable evidence manifest {manifest_path}: {exc}") from exc
    entries = payload.get("entries")
    if not isinstance(entries, list):
        raise EvidenceError(f"evidence manifest {manifest_path} has no `entries` list")
    return [e for e in entries if isinstance(e, dict)]


def load_evidence(
    manifest_path: Path,
    *,
    verify_hashes: bool = True,
) -> Dict[str, EvidenceEntry]:
    """Resolve every manifest entry to an on-disk snapshot, keyed by PMID.

    `path` is resolved relative to the manifest's own directory, which is
    how upstream literature tasks write it (`snapshots/<sha256>`).

    Entries whose snapshot is missing are SKIPPED, not fatal: a manifest
    may legitimately record a source whose bytes were not redistributable.
    They are simply not citable, which is the correct outcome — a citation
    with no retrievable snapshot is an unsupported claim.

    With `verify_hashes`, a snapshot whose bytes do not match the manifest's
    `sha256_binary` raises: that is corruption or substitution, not an
    absence, and must never be silently cited.

    On duplicate PMIDs the first entry in file order wins, so the mapping
    is deterministic regardless of how the manifest was assembled.
    """
    base = manifest_path.parent
    out: Dict[str, EvidenceEntry] = {}
    for entry in load_manifest(manifest_path):
        pmid = str(entry.get("pmid") or "").strip()
        if not pmid and str(entry.get("source_ref_kind") or "") == "pmid":
            pmid = str(entry.get("source_ref") or "").strip()
        if not pmid or pmid in out:
            continue
        rel = str(entry.get("path") or "").strip()
        if not rel:
            continue
        snapshot = base / rel
        if not snapshot.is_file():
            continue
        recorded = str(entry.get("sha256_binary") or "").strip().lower()
        if verify_hashes and recorded:
            actual = sha256_file(snapshot)
            if actual != recorded:
                raise EvidenceError(
                    f"snapshot bytes for PMID {pmid} hash to {actual} but the manifest "
                    f"records {recorded} — refusing to cite a substituted snapshot"
                )
        try:
            raw = snapshot.read_text(encoding="utf-8", errors="replace")
        except OSError as exc:
            raise EvidenceError(f"unreadable snapshot {snapshot}: {exc}") from exc
        out[pmid] = EvidenceEntry(
            pmid=pmid,
            source_ref=str(entry.get("source_ref") or pmid),
            source_kind=str(entry.get("source_kind") or "abstract_only"),
            path=rel,
            sha256_binary=recorded or sha256_file(snapshot),
            retrieval_ts=str(entry.get("retrieval_ts") or ""),
            redistributable=bool(entry.get("redistributable", False)),
            license=str(entry.get("license") or ""),
            text=reference_text(raw),
        )
    return out


def sentences(text: str) -> List[tuple]:
    """Split reference text into `(offset, sentence)` pairs.

    The offset indexes `text` directly, so `text[offset:offset + len(s)]`
    is `s` for every pair — the property `verify_quote` re-checks before a
    quote is written to the matrix.
    """
    out: List[tuple] = []
    cursor = 0
    for part in _SENTENCE_SPLIT.split(text):
        if not part:
            continue
        idx = text.find(part, cursor)
        if idx < 0:  # pragma: no cover — defensive; split() output is a substring
            continue
        out.append((idx, part))
        cursor = idx + len(part)
    return out


def verify_quote(entry: EvidenceEntry, quote: str, offset: int) -> bool:
    """`True` iff `quote` sits verbatim at `offset` in the snapshot text."""
    if not quote:
        return False
    end = offset + len(quote)
    return 0 <= offset and end <= len(entry.text) and entry.text[offset:end] == quote


def entity_pattern(symbol: str) -> Optional[re.Pattern]:
    """Word-boundary matcher for an entity symbol in prose.

    An ALL-CAPS symbol (the HGNC convention, and how gene symbols appear in
    text) is matched case-SENSITIVELY: `CAT`, `SET`, and `MAX` are real gene
    symbols and also ordinary English words, and a case-insensitive match
    would attach a citation to the wrong entity. Mixed-case identifiers
    (region and variant ids, non-human nomenclature) match
    case-insensitively. Symbols shorter than two characters are rejected
    outright — they cannot be matched without unacceptable ambiguity.
    """
    symbol = symbol.strip()
    if len(symbol) < 2:
        return None
    escaped = re.escape(symbol)
    flags = 0 if symbol.isupper() else re.IGNORECASE
    return re.compile(rf"(?<![A-Za-z0-9]){escaped}(?![A-Za-z0-9])", flags)


def mentions(entry: EvidenceEntry, symbol: str) -> List[tuple]:
    """`(offset, sentence)` pairs in `entry` that name `symbol`."""
    pattern = entity_pattern(symbol)
    if pattern is None:
        return []
    return [(off, s) for off, s in sentences(entry.text) if pattern.search(s)]
