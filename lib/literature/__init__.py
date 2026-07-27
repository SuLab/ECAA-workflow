"""Reusable literature-contextualization library.

Shipped alongside the plotting library so a
`contextualize_findings_with_literature` task can map its findings onto
retrieved prior work by CALLING a tested implementation, rather than by
hand-writing a script that embeds PMIDs, quotes, directions, and snapshot
hashes as literals — which is how a deposited run ended up contextualizing
one gene out of 4,033 findings, with every citation's correctness resting
on manual transcription.

Nothing in this package accepts a citation as a literal. A PMID is usable
only if the upstream evidence manifest resolves it to a snapshot on disk
whose bytes hash to the recorded digest; a quote is usable only if it is a
verbatim substring of that snapshot at a recorded offset; a prior
direction is derived from the quoted sentence, and an analysis direction
from the sign of the result table's effect column.

Entry points:

* [`contextualize`][lib.literature.contextualize.contextualize] — read
  inputs, emit `claims_evidence_matrix.csv`, `evidence/manifest.json`, and
  the two declared markdown reports.
* `python3 -m lib.literature.contextualize --help` — the same, as a CLI.

Modality-agnostic: column names are resolved from a declared candidate
list with an explicit override, and `entity_kind` covers gene / region /
variant. No modality, tool, or organism is named in the logic.
"""

from .contextualize import build_rows, contextualize, write_evidence_manifest, write_reports
from .direction import (
    CONCORDANCE_FLAGS,
    DOWN,
    UP,
    DirectionCall,
    concordance,
    effect_direction,
    infer_direction,
)
from .evidence import (
    EvidenceEntry,
    EvidenceError,
    load_evidence,
    mentions,
    reference_text,
    sentences,
    verify_quote,
)
from .matrix import (
    COLUMNS,
    ClaimRow,
    Finding,
    MatrixError,
    ResultTable,
    read_prior_claims,
    read_result_table,
    searched_entities,
    write_matrix,
)

__all__ = [
    "CONCORDANCE_FLAGS",
    "COLUMNS",
    "DOWN",
    "UP",
    "ClaimRow",
    "DirectionCall",
    "EvidenceEntry",
    "EvidenceError",
    "Finding",
    "MatrixError",
    "ResultTable",
    "build_rows",
    "concordance",
    "contextualize",
    "effect_direction",
    "infer_direction",
    "load_evidence",
    "mentions",
    "read_prior_claims",
    "read_result_table",
    "reference_text",
    "searched_entities",
    "sentences",
    "verify_quote",
    "write_evidence_manifest",
    "write_matrix",
    "write_reports",
]
