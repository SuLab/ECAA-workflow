"""Result-table reading, searched-set resolution, and matrix emission.

Everything here is column-name driven with a declared candidate list and
an explicit override — no modality is named in the logic. A DE table
(gene / log2FoldChange / padj), a peak table (region_id / logFC / FDR),
and a variant table (variant_id / beta / p_value) all read through the
same code path.
"""

from __future__ import annotations

import csv
import re
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Sequence, Tuple

from .direction import CONCORDANCE_FLAGS

#: Candidate column names, tried in order, for each role. Overridable per
#: call — these are the fallback when the caller does not declare one.
ID_COLUMNS: Tuple[str, ...] = (
    "finding_id",
    "gene",
    "gene_id",
    "feature",
    "feature_id",
    "variant_id",
    "region_id",
    "peak_id",
    "term",
    "pathway",
    "id",
)
SYMBOL_COLUMNS: Tuple[str, ...] = (
    "symbol",
    "gene_symbol",
    "gene_name",
    "hgnc_symbol",
    "entity",
)
EFFECT_COLUMNS: Tuple[str, ...] = (
    "log2FoldChange",
    "log2FC",
    "log2fc",
    "logFC",
    "lfc",
    "NES",
    "nes",
    "effect",
    "estimate",
    "beta",
)
SIGNIFICANCE_COLUMNS: Tuple[str, ...] = (
    "padj",
    "adj_p_value",
    "adj_p",
    "FDR",
    "fdr",
    "qvalue",
    "q_value",
    "p_value",
    "pvalue",
)

#: Column order of `claims_evidence_matrix.csv`.
#:
#: `analysis_effect` and `analysis_significance` are the canonical,
#: modality-agnostic measurements copied from the upstream finding. Their
#: meaning comes from the upstream result schema and explicit column
#: resolution, not from a gene-expression-specific header alias.
COLUMNS: Tuple[str, ...] = (
    "finding_id",
    "entity",
    "entity_kind",
    "analysis_effect",
    "analysis_significance",
    "analysis_direction",
    "prior_pmid",
    "prior_direction",
    "concordance_flag",
    "evidence_quote",
    "evidence_quote_offset",
    "source_kind",
    "source_hash",
    "retrieval_ts",
    "redistributable",
    "verified",
    "searched",
)

#: Header of the canonical entity-label ↔ accession annotation table this
#: library emits alongside the matrix, in emitted order.
#:
#: The NAMES are fixed because they are the convention the independent
#: label↔accession consistency check resolves against. The SOURCE columns are
#: still resolved by ROLE (`SYMBOL_COLUMNS` → the label,
#: `ID_COLUMNS` → the accession), so a region- or variant-keyed table emits its
#: own identifiers here with no entity-specific handling; `ensembl_gene_id` is
#: then the conventional name for the accession role, not a claim that the
#: value is a gene accession.
SYMBOL_MAP_COLUMNS: Tuple[str, ...] = ("symbol", "ensembl_gene_id")

#: Path of that table, relative to the task output directory (the convention
#: every other declared artifact uses, cf. `evidence/manifest.json`).
#:
#: `annotation/` deliberately, NOT `intermediates/`: the deposit exporter drops
#: every path carrying an `intermediates` component as reproducible bloat, so a
#: map written there is absent from each exported deposit and the consistency
#: obligation can never be discharged in the artifact a reader receives.
SYMBOL_MAP_RELPATH: str = "annotation/symbol_map.tsv"

#: Cell values that mean "no value", compared case-insensitively after
#: stripping. A mapping row is only emitted when BOTH sides carry a value, so
#: an unresolved identifier is absent from the map rather than present with an
#: empty cell that a reader could mistake for a real binding.
_MISSING_CELLS = frozenset({"", "na", "n/a", "nan", "null", "none", "-", "."})

_TOKEN = re.compile(r"[A-Za-z0-9]+")


class MatrixError(RuntimeError):
    """Raised when the inputs cannot support a well-formed matrix."""


@dataclass
class Finding:
    """One row of the upstream result table, reduced to what mapping needs."""

    finding_id: str
    symbol: str
    effect: Optional[float]
    significance: Optional[float]
    row_index: int


@dataclass
class ClaimRow:
    """One `(finding, prior source)` pair — one line of the output matrix."""

    finding_id: str
    entity: str
    entity_kind: str
    analysis_effect: str
    analysis_significance: str
    analysis_direction: str
    prior_pmid: str
    prior_direction: str
    concordance_flag: str
    evidence_quote: str
    evidence_quote_offset: int
    source_kind: str
    source_hash: str
    retrieval_ts: str
    redistributable: str
    verified: str
    searched: str

    def __post_init__(self) -> None:
        if self.concordance_flag not in CONCORDANCE_FLAGS:
            raise MatrixError(
                f"concordance_flag {self.concordance_flag!r} is outside the closed set "
                f"{list(CONCORDANCE_FLAGS)}"
            )
        if not self.finding_id:
            raise MatrixError("every claim row must carry a non-empty finding_id")


@dataclass
class ResultTable:
    """A parsed upstream result table plus the columns it resolved to."""

    findings: List[Finding] = field(default_factory=list)
    id_column: str = ""
    symbol_column: Optional[str] = None
    effect_column: Optional[str] = None
    significance_column: Optional[str] = None
    n_rows: int = 0
    n_significant: int = 0


def _resolve(
    header: Sequence[str],
    declared: Optional[str],
    candidates: Sequence[str],
) -> Optional[str]:
    """First of `declared` then `candidates` that is present in `header`.

    A DECLARED column that is absent is an error, not a fallback: silently
    ranging over candidates after the caller named a column would attach
    citations to a different measurement than the one requested.
    """
    if declared:
        if declared not in header:
            raise MatrixError(
                f"declared column {declared!r} not in result table header {list(header)}"
            )
        return declared
    return next((c for c in candidates if c in header), None)


def _float(raw: Optional[str]) -> Optional[float]:
    if raw is None:
        return None
    try:
        value = float(raw.strip())
    except (TypeError, ValueError, AttributeError):
        return None
    return value if value == value and abs(value) != float("inf") else None


def _present(raw: Optional[str]) -> Optional[str]:
    """`raw` stripped, or `None` when it is absent or a missing-value marker."""
    if raw is None:
        return None
    value = raw.strip()
    return None if value.lower() in _MISSING_CELLS else value


def _sniff_delimiter(path: Path) -> str:
    """`\\t` for `.tsv`/`.txt`, `,` otherwise, overridden by whichever
    character actually appears in the header line."""
    try:
        first = path.open("r", encoding="utf-8").readline()
    except OSError as exc:
        raise MatrixError(f"unreadable result table {path}: {exc}") from exc
    if "\t" in first:
        return "\t"
    if "," in first:
        return ","
    return "\t" if path.suffix.lower() in (".tsv", ".txt") else ","


def read_result_table(
    path: Path,
    *,
    id_column: Optional[str] = None,
    symbol_column: Optional[str] = None,
    effect_column: Optional[str] = None,
    significance_column: Optional[str] = None,
    threshold: Optional[float] = 0.05,
    significant_only: bool = True,
) -> ResultTable:
    """Read the upstream findings table.

    `threshold` filters on the resolved significance column (strictly less
    than, the FDR convention). `threshold=None` or an unresolvable
    significance column means every row is a finding — an honest reduced
    contract, not an empty result.
    """
    delimiter = _sniff_delimiter(path)
    with path.open("r", encoding="utf-8", newline="") as fh:
        reader = csv.DictReader(fh, delimiter=delimiter)
        header = list(reader.fieldnames or [])
        if not header:
            raise MatrixError(f"result table {path} has no header row")
        id_col = _resolve(header, id_column, ID_COLUMNS)
        if id_col is None:
            raise MatrixError(
                f"no row-identifier column in {path}; tried {list(ID_COLUMNS)} over {header}"
            )
        sym_col = _resolve(header, symbol_column, SYMBOL_COLUMNS)
        eff_col = _resolve(header, effect_column, EFFECT_COLUMNS)
        sig_col = _resolve(header, significance_column, SIGNIFICANCE_COLUMNS)

        table = ResultTable(
            id_column=id_col,
            symbol_column=sym_col,
            effect_column=eff_col,
            significance_column=sig_col,
        )
        for row_index, row in enumerate(reader):
            finding_id = (row.get(id_col) or "").strip()
            if not finding_id:
                continue
            table.n_rows += 1
            significance = _float(row.get(sig_col)) if sig_col else None
            passes = True
            if significant_only and threshold is not None and sig_col is not None:
                passes = significance is not None and significance < threshold
            if passes:
                table.n_significant += 1
            else:
                continue
            symbol = _present(row.get(sym_col)) if sym_col else None
            table.findings.append(
                Finding(
                    finding_id=finding_id,
                    # No symbol column → the row identifier IS the entity
                    # label. The library never maps ids to symbols itself:
                    # identifier resolution belongs to the run's pinned
                    # annotation, which the caller joins in beforehand.
                    symbol=symbol or finding_id,
                    effect=_float(row.get(eff_col)) if eff_col else None,
                    significance=significance,
                    row_index=row_index,
                )
            )
    # Deterministic order regardless of how the upstream table was sorted.
    table.findings.sort(key=lambda f: (f.finding_id, f.row_index))
    return table


def read_prior_claims(path: Path) -> List[dict]:
    """Read `prior_claims_matrix.csv` into dict rows (order preserved)."""
    delimiter = _sniff_delimiter(path)
    with path.open("r", encoding="utf-8", newline="") as fh:
        return [dict(r) for r in csv.DictReader(fh, delimiter=delimiter)]


#: Columns of the prior-claims matrix scanned for the entity names a query
#: was issued for. `axis` is the query-axis id upstream literature tasks
#: write (e.g. `dusp1_dexamethasone_asm`).
QUERY_COLUMNS: Tuple[str, ...] = ("axis", "query", "query_terms", "candidate_method")


def searched_entities(
    prior_rows: Iterable[dict],
    candidate_symbols: Iterable[str],
    *,
    explicit: Optional[Iterable[str]] = None,
) -> Dict[str, List[str]]:
    """Which entities a literature query was actually issued for.

    This is the `not_assessed` / `no_prior_finding` boundary, and the atom
    is emphatic that the two must not be conflated — so the searched set is
    never inferred from "we happen to have a paper mentioning it".

    Resolution, in order:

    1. `explicit` — a caller-supplied list (the search scope the retrieval
       step recorded). Authoritative when present.
    2. Otherwise, an entity is searched iff its symbol appears as a token of
       one of the `QUERY_COLUMNS` in some prior-claims row. Query axes are
       named after what was queried, so this recovers the scope from the
       upstream artifact rather than from the analyst's memory.

    Returns `symbol -> [axis values that named it]`, empty when nothing
    resolves. An empty result means everything is `not_assessed`, which is
    the correct answer when no evidence of a query survives: the library
    will not manufacture a searched set in order to produce a prettier
    matrix.
    """
    symbols = {s for s in candidate_symbols if s}
    if explicit is not None:
        wanted = {s.strip() for s in explicit if s and s.strip()}
        return {s: [] for s in sorted(symbols & wanted)}

    by_symbol: Dict[str, List[str]] = {}
    lowered = {s.lower(): s for s in symbols}
    for row in prior_rows:
        for column in QUERY_COLUMNS:
            raw = (row.get(column) or "").strip()
            if not raw:
                continue
            for token in _TOKEN.findall(raw.lower()):
                symbol = lowered.get(token)
                if symbol is None:
                    continue
                axis = (row.get("axis") or raw).strip()
                bucket = by_symbol.setdefault(symbol, [])
                if axis not in bucket:
                    bucket.append(axis)
    return {k: sorted(v) for k, v in sorted(by_symbol.items())}


def prior_pmids(prior_rows: Iterable[dict]) -> List[str]:
    """Every PMID named by the prior-claims matrix, de-duplicated, sorted."""
    found = set()
    for row in prior_rows:
        pmid = (row.get("pmid") or "").strip()
        if not pmid and (row.get("source_ref_kind") or "").strip() == "pmid":
            pmid = (row.get("source_ref") or "").strip()
        if pmid:
            found.add(pmid)
    return sorted(found)


def pmids_for_axes(prior_rows: Iterable[dict], axes: Sequence[str]) -> List[str]:
    """PMIDs retrieved under any of `axes`, de-duplicated and sorted.

    Restricting an entity's candidate sources to the axes that actually
    named it stops a citation retrieved for one query being attached to an
    entity that query never mentioned.
    """
    wanted = {a for a in axes if a}
    found = set()
    for row in prior_rows:
        if wanted and (row.get("axis") or "").strip() not in wanted:
            continue
        pmid = (row.get("pmid") or "").strip()
        if not pmid and (row.get("source_ref_kind") or "").strip() == "pmid":
            pmid = (row.get("source_ref") or "").strip()
        if pmid:
            found.add(pmid)
    return sorted(found)


def write_matrix(rows: Sequence[ClaimRow], out_path: Path) -> None:
    """Write `claims_evidence_matrix.csv`.

    Rows are emitted in `(finding_id, prior_pmid)` order so the file is
    byte-identical across runs over identical inputs.
    """
    out_path.parent.mkdir(parents=True, exist_ok=True)
    ordered = sorted(rows, key=lambda r: (r.finding_id, r.prior_pmid, r.concordance_flag))
    with out_path.open("w", encoding="utf-8", newline="") as fh:
        writer = csv.DictWriter(fh, fieldnames=list(COLUMNS), lineterminator="\n")
        writer.writeheader()
        for row in ordered:
            writer.writerow({k: asdict(row)[k] for k in COLUMNS})


def symbol_map_pairs(
    path: Path,
    *,
    id_column: Optional[str] = None,
    symbol_column: Optional[str] = None,
) -> List[Tuple[str, str]]:
    """Distinct `(entity label, accession)` pairs the table at `path` carries.

    Both sides are resolved by ROLE from the same candidate lists the matrix
    build uses — `SYMBOL_COLUMNS` for the label, `ID_COLUMNS` for the
    accession — so this is the mapping the run's own identity join actually
    supplied, never one this library invents. A declared column that is absent
    from the header raises, exactly as it does for the matrix.

    Returns `[]` when the table resolves only ONE of the two roles (or both to
    the same column): that is the honest answer for an input carrying no
    accession — or no label — column, and the caller writes a header-only map
    rather than fabricating bindings.

    Rows are skipped when either side is missing / a missing-value marker, and
    when the two sides are equal (an identity pair records no mapping). Every
    row of the table is considered, not just the significant set: the mapping is
    a property of the input annotation, not of a significance threshold.

    Ordered by `(accession, label)`, so the emitted file is byte-identical
    across runs over identical input regardless of the table's own row order.
    """
    delimiter = _sniff_delimiter(path)
    with path.open("r", encoding="utf-8", newline="") as fh:
        reader = csv.DictReader(fh, delimiter=delimiter)
        header = list(reader.fieldnames or [])
        if not header:
            raise MatrixError(f"result table {path} has no header row")
        accession_col = _resolve(header, id_column, ID_COLUMNS)
        label_col = _resolve(header, symbol_column, SYMBOL_COLUMNS)
        if accession_col is None or label_col is None or accession_col == label_col:
            return []
        pairs = set()
        for row in reader:
            accession = _present(row.get(accession_col))
            label = _present(row.get(label_col))
            if accession is None or label is None or accession == label:
                continue
            pairs.add((label, accession))
    return sorted(pairs, key=lambda pair: (pair[1], pair[0]))


def write_symbol_map(pairs: Sequence[Tuple[str, str]], out_path: Path) -> Path:
    """Write the declared entity-label ↔ accession annotation table.

    This is the evidence the independent label↔accession consistency check
    needs: a table carrying BOTH roles in named columns, at a stable path the
    deposit export keeps. Emitting it as a DECLARED artifact is what stops the
    check from having to scavenge whichever plausibly-shaped file a run happens
    to have left behind.

    An empty `pairs` writes the header alone — an honest empty map, from which
    no binding can be read, rather than a silently absent file that a
    completion check would report as a missing artifact.
    """
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w", encoding="utf-8", newline="") as fh:
        writer = csv.writer(fh, delimiter="\t", lineterminator="\n")
        writer.writerow(list(SYMBOL_MAP_COLUMNS))
        for label, accession in pairs:
            writer.writerow([label, accession])
    return out_path
