"""Generic literature contextualization: findings × retrieved evidence.

This is the reusable implementation of the
`contextualize_findings_with_literature` atom. It maps every row of an
upstream result table onto the prior-work sources an upstream literature
task actually retrieved, and emits `claims_evidence_matrix.csv` plus this
task's own `evidence/manifest.json`, `annotation/symbol_map.tsv` and the two
markdown reports the atom declares.

Why it exists
-------------
The per-run alternative is a hand-written script, and a deposited run
showed what that produces: four PMIDs, four verbatim quotes, four
direction labels, and four sha256 digests typed as literals into the
script body, contextualizing exactly one gene. The other 4,029 findings
were `not_assessed`, and every citation's correctness rested on the
analyst having transcribed the right quote next to the right hash. None
of it was recomputable, and none of it generalized to the next run.

Here, nothing is embedded:

* **PMIDs** come from the upstream `prior_claims_matrix.csv` and must
  resolve to an entry in the upstream `evidence/manifest.json` whose
  snapshot is on disk and whose bytes hash to the recorded digest.
* **Quotes** are extracted from that snapshot — the sentence naming the
  entity — and re-verified as a verbatim substring at a recorded offset
  before being written.
* **Directions** come from the result table (analysis side, sign of the
  effect column) and from a lexical scan of the quoted sentence (prior
  side). Neither is supplied by the caller.
* **Concordance flags** come from the closed set, with the
  retrieval-shaped ones (`no_prior_finding` / `not_assessed`) assigned
  strictly by whether a query naming the entity was issued.

What it does not claim
----------------------
A derived prior direction means "this retrieved sentence reports an
increase/decrease involving this entity". It does not mean the prior
measured the same contrast. Pass `contrast_terms` to require the
contrast vocabulary in the sentence; the emitted
`citation_verification_report.md` states, per row, whether that grounding
was applied. Rows are evidence for a reader to judge, not a verdict.

Usage
-----
    python3 -m runtime.literature \\
        --results runtime/outputs/differential_expression/de_results.tsv \\
        --symbol-column symbol \\
        --prior-claims runtime/outputs/review_prior_work/prior_claims_matrix.csv \\
        --evidence-manifest runtime/outputs/review_prior_work/evidence/manifest.json \\
        --out-dir runtime/outputs/contextualize_findings_with_literature \\
        --entity-kind gene \\
        --contrast-term dexamethasone --contrast-term glucocorticoid

Deterministic: pure over its inputs, no clock, no network, no randomness.
`retrieval_ts` values are copied from the upstream manifest, never
generated.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path
from typing import Dict, List, Optional, Sequence

from .direction import (
    DirectionCall,
    concordance,
    effect_direction,
    infer_direction,
)
from .evidence import EvidenceEntry, load_evidence, mentions, verify_quote
from .matrix import (
    SYMBOL_MAP_RELPATH,
    ClaimRow,
    Finding,
    MatrixError,
    ResultTable,
    pmids_for_axes,
    read_prior_claims,
    read_result_table,
    searched_entities,
    symbol_map_pairs,
    write_matrix,
    write_symbol_map,
)

ENTITY_KINDS = ("gene", "region", "variant")


def _fmt_effect(value: Optional[float]) -> str:
    """Fixed 6-decimal rendering so the column is byte-stable."""
    return "" if value is None else f"{value:.6f}"


def _best_evidence(
    entry: EvidenceEntry,
    symbol: str,
    contrast_terms: Sequence[str],
) -> Optional[tuple]:
    """Pick the sentence to cite for `symbol` in one snapshot.

    Preference order over the sentences naming the entity:
    contrast-grounded and directional, then directional, then
    contrast-grounded, then the first mention. Ties break on the earlier
    offset, so the choice is a function of the snapshot text alone.

    Returns `(offset, sentence, DirectionCall)`, or `None` when the
    snapshot never names the entity.
    """
    hits = mentions(entry, symbol)
    if not hits:
        return None
    scored = []
    for offset, sentence in hits:
        call = infer_direction(sentence, contrast_terms=contrast_terms)
        rank = (
            0 if (call.direction and call.contrast_grounded)
            else 1 if call.direction
            else 2 if call.contrast_grounded
            else 3
        )
        scored.append((rank, offset, sentence, call))
    scored.sort(key=lambda s: (s[0], s[1]))
    _rank, offset, sentence, call = scored[0]
    return offset, sentence, call


def _row_for_pair(
    finding: Finding,
    entity_kind: str,
    analysis_direction: Optional[str],
    entry: EvidenceEntry,
    offset: int,
    quote: str,
    call: DirectionCall,
) -> ClaimRow:
    verified = verify_quote(entry, quote, offset)
    if not verified:
        # A quote that does not sit at its recorded offset is not evidence.
        # Emit the row without it rather than shipping an unverifiable
        # citation that the downstream substring check would reject.
        quote, offset = "", 0
    prior_direction = call.direction if verified else None
    flag = concordance(analysis_direction, prior_direction)
    return ClaimRow(
        finding_id=finding.finding_id,
        entity=finding.symbol,
        entity_kind=entity_kind,
        analysis_effect=_fmt_effect(finding.effect),
        analysis_log2fc=_fmt_effect(finding.effect),
        analysis_direction=analysis_direction or "",
        prior_pmid=entry.pmid,
        prior_direction=prior_direction or "",
        concordance_flag=flag,
        evidence_quote=quote,
        evidence_quote_offset=offset,
        source_kind=entry.source_kind,
        source_hash=entry.source_hash,
        retrieval_ts=entry.retrieval_ts,
        redistributable="true" if entry.redistributable else "false",
        verified="true" if verified else "false",
        searched="true",
    )


def _unsearched_row(
    finding: Finding, entity_kind: str, analysis_direction: Optional[str]
) -> ClaimRow:
    return ClaimRow(
        finding_id=finding.finding_id,
        entity=finding.symbol,
        entity_kind=entity_kind,
        analysis_effect=_fmt_effect(finding.effect),
        analysis_log2fc=_fmt_effect(finding.effect),
        analysis_direction=analysis_direction or "",
        prior_pmid="",
        prior_direction="",
        concordance_flag="not_assessed",
        evidence_quote="",
        evidence_quote_offset=0,
        source_kind="none",
        source_hash="none",
        retrieval_ts="",
        redistributable="false",
        verified="false",
        searched="false",
    )


def _no_prior_row(
    finding: Finding, entity_kind: str, analysis_direction: Optional[str]
) -> ClaimRow:
    row = _unsearched_row(finding, entity_kind, analysis_direction)
    row.concordance_flag = "no_prior_finding"
    row.searched = "true"
    return row


def build_rows(
    table: ResultTable,
    prior_rows: Sequence[dict],
    evidence: Dict[str, EvidenceEntry],
    *,
    entity_kind: str = "gene",
    contrast_terms: Sequence[str] = (),
    explicit_searched: Optional[Sequence[str]] = None,
) -> List[ClaimRow]:
    """Map every finding onto its retrieved prior sources.

    One row per `(finding, cited PMID)`; one row per finding when nothing
    was cited. Findings whose entity no query named are `not_assessed`;
    searched findings with no snapshot naming them are `no_prior_finding`.
    """
    if entity_kind not in ENTITY_KINDS:
        raise MatrixError(f"entity_kind must be one of {list(ENTITY_KINDS)}, got {entity_kind!r}")

    scope = searched_entities(
        prior_rows,
        (f.symbol for f in table.findings),
        explicit=explicit_searched,
    )

    rows: List[ClaimRow] = []
    for finding in table.findings:
        analysis_direction = effect_direction(finding.effect)
        axes = scope.get(finding.symbol)
        if axes is None:
            rows.append(_unsearched_row(finding, entity_kind, analysis_direction))
            continue

        # Only sources retrieved under an axis that named this entity are
        # candidates; an explicit scope carries no axes, so every retrieved
        # source is a candidate for it.
        candidate_pmids = pmids_for_axes(prior_rows, axes) if axes else sorted(evidence)
        cited = 0
        for pmid in candidate_pmids:
            entry = evidence.get(pmid)
            if entry is None:
                # Named in the prior-claims matrix but with no retrievable
                # snapshot: not citable. Dropping it is the atom's rule.
                continue
            best = _best_evidence(entry, finding.symbol, contrast_terms)
            if best is None:
                continue
            offset, sentence, call = best
            rows.append(
                _row_for_pair(
                    finding, entity_kind, analysis_direction, entry, offset, sentence, call
                )
            )
            cited += 1
        if cited == 0:
            rows.append(_no_prior_row(finding, entity_kind, analysis_direction))
    return rows


def write_evidence_manifest(
    rows: Sequence[ClaimRow],
    evidence: Dict[str, EvidenceEntry],
    upstream_manifest: Path,
    out_dir: Path,
    *,
    copy_snapshots: bool = True,
) -> Path:
    """Write this task's `evidence/manifest.json` for the cited PMIDs only.

    Downstream validation resolves a matrix PMID against THIS task's
    manifest, so every PMID the matrix cites must appear here. Snapshots
    are copied so the task's evidence directory is self-contained and the
    recorded `path` resolves relative to the manifest, exactly as the
    upstream one does.
    """
    cited = sorted({r.prior_pmid for r in rows if r.prior_pmid})
    upstream_entries = {}
    try:
        payload = json.loads(upstream_manifest.read_text(encoding="utf-8"))
        for entry in payload.get("entries", []):
            pmid = str(entry.get("pmid") or entry.get("source_ref") or "").strip()
            if pmid:
                upstream_entries.setdefault(pmid, entry)
    except (OSError, json.JSONDecodeError):
        upstream_entries = {}

    evidence_dir = out_dir / "evidence"
    snapshots_dir = evidence_dir / "snapshots"
    entries = []
    for pmid in cited:
        source = evidence.get(pmid)
        if source is None:
            continue
        entry = dict(upstream_entries.get(pmid, {}))
        rel = f"snapshots/{source.sha256_binary}"
        if copy_snapshots:
            snapshots_dir.mkdir(parents=True, exist_ok=True)
            src = upstream_manifest.parent / source.path
            dest = snapshots_dir / source.sha256_binary
            if src.is_file() and not dest.exists():
                shutil.copyfile(src, dest)
        entry["path"] = rel
        entry.setdefault("pmid", pmid)
        entry.setdefault("source_kind", source.source_kind)
        entry.setdefault("sha256_binary", source.sha256_binary)
        entry.setdefault("retrieval_ts", source.retrieval_ts)
        entry.setdefault("redistributable", source.redistributable)
        entry.setdefault("license", source.license)
        entries.append(entry)

    evidence_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = evidence_dir / "manifest.json"
    manifest_path.write_text(
        json.dumps({"schema_version": 2, "entries": entries}, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return manifest_path


def _counts(rows: Sequence[ClaimRow]) -> Dict[str, int]:
    out: Dict[str, int] = {}
    for row in rows:
        out[row.concordance_flag] = out.get(row.concordance_flag, 0) + 1
    return dict(sorted(out.items()))


#: One quotable sentence per emitted count, so a narrative can state the count
#: WITH its denominator instead of inferring one. A deposited report read the 9
#: assessed evidence ROWS as "9 specific genes" when only 4 entities were ever
#: searched, then contradicted itself 109 lines later with the correct 4 — the
#: two numbers were conflatable because neither carried its own definition.
COUNT_DEFINITIONS: Dict[str, str] = {
    "n_entities_assessed": (
        "distinct entities a prior-work query was actually issued for. This is the "
        "only count that may be described as a number of entities searched."
    ),
    "n_entities_not_assessed": (
        "distinct entities NO prior-work query was issued for. Not novel, and not "
        "'no prior work' — their literature status is unknown."
    ),
    "n_evidence_rows_assessed": (
        "rows of claims_evidence_matrix.csv with searched=true. One assessed entity "
        "contributes one row PER cited source, so this is always >= "
        "n_entities_assessed and is NEVER a count of entities."
    ),
    "n_evidence_rows_total": (
        "total rows of claims_evidence_matrix.csv, assessed and not. A count of rows, "
        "not of entities."
    ),
    "n_search_axes_total": (
        "distinct query axes the upstream prior-work retrieval recorded, including "
        "axes that name a method, a dataset, or a condition rather than an entity."
    ),
    "n_search_axes_naming_an_assessed_entity": (
        "the subset of those axes that named one of the assessed entities. Only this "
        "subset supports a statement about how many entities were searched."
    ),
}


def assessment_counts(
    rows: Sequence[ClaimRow],
    prior_rows: Sequence[dict],
    scope: Dict[str, List[str]],
    *,
    scope_source: str,
) -> Dict[str, object]:
    """Self-describing assessment counts, each with its own denominator named.

    Entity counts and evidence-ROW counts are separate keys with separate names
    because they differ whenever one entity cites more than one source, and a
    narrative that reads a row count as an entity count states a false number of
    searched entities. Axis counts are likewise split into the total the upstream
    retrieval recorded and the subset that named an assessed entity, because a
    retrieval axis may name a method or a dataset rather than an entity.

    Every count is derived from the emitted artifacts — `rows` is what
    `claims_evidence_matrix.csv` will contain, `prior_rows` is the upstream
    prior-claims matrix — so a reader can reconcile each number against the file
    it came from. `COUNT_DEFINITIONS` is emitted alongside so a report can quote
    a count's definition rather than infer one.

    Pure + deterministic; entity and axis lists are sorted.
    """
    assessed_entities = sorted({r.entity for r in rows if r.searched == "true" and r.entity})
    not_assessed_entities = sorted(
        {r.entity for r in rows if r.searched != "true" and r.entity} - set(assessed_entities)
    )
    axes_total = sorted({(row.get("axis") or "").strip() for row in prior_rows} - {""})
    naming_axes = sorted(
        {axis for entity in assessed_entities for axis in scope.get(entity, []) if axis}
    )
    return {
        "n_entities_assessed": len(assessed_entities),
        "n_entities_not_assessed": len(not_assessed_entities),
        "n_evidence_rows_assessed": sum(1 for r in rows if r.searched == "true"),
        "n_evidence_rows_total": len(rows),
        "n_search_axes_total": len(axes_total),
        "n_search_axes_naming_an_assessed_entity": len(naming_axes),
        "entities_assessed": assessed_entities,
        "search_axes_naming_an_assessed_entity": naming_axes,
        # `explicit` means the retrieval step handed us its own scope list, so no
        # axis named the entities and the axis-naming count is 0 by construction
        # rather than by absence of a query. Recorded so a reader never reads
        # that 0 as "nothing was searched".
        "search_scope_source": scope_source,
        "count_definitions": dict(COUNT_DEFINITIONS),
    }


def write_reports(
    rows: Sequence[ClaimRow],
    table: ResultTable,
    evidence: Dict[str, EvidenceEntry],
    out_dir: Path,
    *,
    contrast_terms: Sequence[str] = (),
) -> None:
    """Write `literature_search_protocol.md` + `citation_verification_report.md`.

    Both describe what this run actually did, computed from the rows — no
    narrative, no synthesis, nothing the matrix does not already contain.
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    counts = _counts(rows)
    cited = sorted({r.prior_pmid for r in rows if r.prior_pmid})
    verified = sum(1 for r in rows if r.verified == "true")
    grounded = "yes" if contrast_terms else "no"
    no_symbol = "(none — identifier used as the entity label)"
    no_effect = "(none — no analysis direction available)"
    no_significance = "(none — every row treated as a finding)"

    protocol = [
        "# Literature search protocol",
        "",
        "Contextualization performed by `lib/literature` (deterministic, offline).",
        "No source was retrieved by this task: every citation resolves to a",
        "snapshot an upstream literature task had already stored.",
        "",
        "## Inputs",
        "",
        f"- Result table column for the row identifier: `{table.id_column}`",
        f"- Symbol column: `{table.symbol_column or no_symbol}`",
        f"- Signed-effect column: `{table.effect_column or no_effect}`",
        f"- Significance column: `{table.significance_column or no_significance}`",
        f"- Findings considered: {len(table.findings)} of {table.n_rows} table rows",
        f"- Retrievable upstream snapshots: {len(evidence)}",
        "",
        "## Procedure",
        "",
        "1. An entity is *searched* iff a prior-claims query axis names it.",
        "   Entities outside that set are `not_assessed`, never `no_prior_finding`.",
        "2. For each searched entity, each PMID retrieved under a naming axis is",
        "   opened and scanned for sentences containing the entity symbol",
        "   (word-boundary; case-sensitive for ALL-CAPS symbols).",
        "3. The cited sentence is the highest-ranked mention (contrast-grounded and",
        "   directional > directional > contrast-grounded > first), re-verified as a",
        "   verbatim substring of the snapshot at the recorded offset.",
        "4. The prior direction is a lexical call over that sentence; the analysis",
        "   direction is the sign of the effect column. Concordance is their",
        "   comparison; an unresolved direction on either side is `unverifiable`.",
        "",
        f"- Contrast terms required in the cited sentence: {grounded}"
        + (f" ({', '.join(contrast_terms)})" if contrast_terms else ""),
        "",
    ]
    (out_dir / "literature_search_protocol.md").write_text("\n".join(protocol), encoding="utf-8")

    report = [
        "# Citation verification report",
        "",
        f"- Matrix rows: {len(rows)}",
        f"- Rows with a quote verified against its snapshot: {verified}",
        f"- Distinct PMIDs cited: {len(cited)}",
        "",
        "## Concordance flags",
        "",
    ]
    report += [f"- `{flag}`: {n}" for flag, n in counts.items()]
    report += [
        "",
        "## Integrity",
        "",
        "Every cited snapshot was byte-hashed and compared against the",
        "`sha256_binary` its manifest recorded before any quote was taken from it.",
        "Every emitted quote was re-checked as a verbatim substring at its",
        "recorded offset; a quote failing that check is dropped and its row",
        "emitted without evidence rather than with an unverifiable citation.",
        "",
        "## Interpretation boundary",
        "",
        "A `same_direction` / `opposite_direction` flag compares the SIGN of this",
        "analysis' effect against the direction a retrieved sentence reports for",
        "the same entity. It does not establish that the prior measured the same",
        "contrast, tissue, or perturbation.",
        (
            "Cited sentences were required to mention at least one contrast term "
            f"({', '.join(contrast_terms)}), which constrains but does not prove "
            "contrast equivalence."
            if contrast_terms
            else "No contrast terms were supplied, so cited sentences were NOT required "
            "to mention the analysis contrast. Treat these flags as pointers to "
            "relevant prior text, not as replication verdicts."
        ),
        "",
    ]
    (out_dir / "citation_verification_report.md").write_text("\n".join(report), encoding="utf-8")


def contextualize(
    results: Path,
    prior_claims: Path,
    evidence_manifest: Path,
    out_dir: Path,
    *,
    entity_kind: str = "gene",
    id_column: Optional[str] = None,
    symbol_column: Optional[str] = None,
    effect_column: Optional[str] = None,
    significance_column: Optional[str] = None,
    threshold: Optional[float] = 0.05,
    significant_only: bool = True,
    contrast_terms: Sequence[str] = (),
    explicit_searched: Optional[Sequence[str]] = None,
    verify_hashes: bool = True,
) -> Dict[str, object]:
    """Run the whole contextualization and write every declared artifact.

    Returns the summary dict the caller folds into its `result.json`.
    """
    table = read_result_table(
        results,
        id_column=id_column,
        symbol_column=symbol_column,
        effect_column=effect_column,
        significance_column=significance_column,
        threshold=threshold,
        significant_only=significant_only,
    )
    prior_rows = read_prior_claims(prior_claims)
    evidence = load_evidence(evidence_manifest, verify_hashes=verify_hashes)

    rows = build_rows(
        table,
        prior_rows,
        evidence,
        entity_kind=entity_kind,
        contrast_terms=contrast_terms,
        explicit_searched=explicit_searched,
    )

    out_dir.mkdir(parents=True, exist_ok=True)
    write_matrix(rows, out_dir / "claims_evidence_matrix.csv")
    write_evidence_manifest(rows, evidence, evidence_manifest, out_dir)
    write_reports(rows, table, evidence, out_dir, contrast_terms=contrast_terms)

    # The label↔accession mapping the input carried, emitted as a declared
    # artifact at a deposit-safe path so the independent consistency check reads
    # a known table instead of scavenging the output tree for a plausible one.
    # Written unconditionally (header-only when the input pairs no accession
    # with a label) so its absence always means the task did not run the
    # library, never that the input happened to lack a column.
    mapping = symbol_map_pairs(results, id_column=id_column, symbol_column=symbol_column)
    write_symbol_map(mapping, out_dir / SYMBOL_MAP_RELPATH)

    # Same pure call `build_rows` makes, so the axis attribution the counts
    # report is the one the rows were built from.
    scope = searched_entities(
        prior_rows,
        (f.symbol for f in table.findings),
        explicit=explicit_searched,
    )

    summary: Dict[str, object] = {
        "n_findings": len(table.findings),
        "n_rows": len(rows),
        "n_snapshots_available": len(evidence),
        "cited_pmids": sorted({r.prior_pmid for r in rows if r.prior_pmid}),
        "concordance_counts": _counts(rows),
        "contrast_terms": list(contrast_terms),
        "columns": {
            "id": table.id_column,
            "symbol": table.symbol_column,
            "effect": table.effect_column,
            "significance": table.significance_column,
        },
        "symbol_map": {"path": SYMBOL_MAP_RELPATH, "n_rows": len(mapping)},
    }
    # Additive: the pre-existing keys above keep their exact meanings; these
    # carry their own denominators so an entity count and a row count can no
    # longer be read as the same number.
    summary.update(
        assessment_counts(
            rows,
            prior_rows,
            scope,
            scope_source="explicit" if explicit_searched is not None else "query_axes",
        )
    )
    return summary


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        # Derived from the parent package so the help text names the
        # invocation that actually works: `runtime.literature` in a shipped
        # package, `lib.literature` in the repo.
        prog=f"python3 -m {__package__ or 'lib.literature'}",
        description="Map analysis findings onto retrieved prior literature.",
    )
    p.add_argument("--results", type=Path, required=True, help="upstream result table (TSV/CSV)")
    p.add_argument("--prior-claims", type=Path, required=True, help="prior_claims_matrix.csv")
    p.add_argument(
        "--evidence-manifest",
        type=Path,
        required=True,
        help="upstream evidence/manifest.json",
    )
    p.add_argument("--out-dir", type=Path, required=True, help="this task's output directory")
    p.add_argument("--entity-kind", choices=ENTITY_KINDS, default="gene")
    p.add_argument("--id-column", default=None)
    p.add_argument("--symbol-column", default=None)
    p.add_argument("--effect-column", default=None)
    p.add_argument("--significance-column", default=None)
    p.add_argument("--threshold", type=float, default=0.05)
    p.add_argument(
        "--all-rows",
        action="store_true",
        help="contextualize every table row, not just the significant set",
    )
    p.add_argument(
        "--contrast-term",
        action="append",
        default=[],
        dest="contrast_terms",
        help="require a cited sentence to mention this term (repeatable)",
    )
    p.add_argument(
        "--searched-entities",
        type=Path,
        default=None,
        help="file of entity symbols a query was issued for, one per line",
    )
    p.add_argument(
        "--no-verify-hashes",
        action="store_true",
        help="skip snapshot byte-hash verification (diagnostics only)",
    )
    return p


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    explicit = None
    if args.searched_entities is not None:
        explicit = [
            line.strip()
            for line in args.searched_entities.read_text(encoding="utf-8").splitlines()
            if line.strip() and not line.startswith("#")
        ]
    summary = contextualize(
        args.results,
        args.prior_claims,
        args.evidence_manifest,
        args.out_dir,
        entity_kind=args.entity_kind,
        id_column=args.id_column,
        symbol_column=args.symbol_column,
        effect_column=args.effect_column,
        significance_column=args.significance_column,
        threshold=args.threshold,
        significant_only=not args.all_rows,
        contrast_terms=args.contrast_terms,
        explicit_searched=explicit,
        verify_hashes=not args.no_verify_hashes,
    )
    json.dump(summary, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
