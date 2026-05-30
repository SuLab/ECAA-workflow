#!/usr/bin/env python3
"""Adversarial claim-verification injection harness.

Drives the live chat server's POST /task/:id/verify endpoint with a battery
of crafted narrative + result-table pairs and asserts the deterministic
claim_extractor + claim_verifier pipeline classifies each claim correctly.

The focus is *false claims not backed by the underlying data*: sign flips,
magnitude fabrication, p-value fabrication, threshold violations, fabricated
entities, rank-membership lies, and a wide spread of edge cases (scientific
notation, unicode, tolerance boundaries, ambiguous table refs, malformed CSV,
direction synonyms, exclude patterns, etc.).

Usage:
    python3 scripts/claim_verify_injection_harness.py \
        --session <session_id> --package <emitted_package_path> [--task reporting]

Each case clears the task narrative dir + results/tables, writes the case's
files, POSTs verify, and checks per-entity verdicts (subset) plus optional
exact (verified, mismatch, unverifiable) counts. Exit code 0 iff all pass.
"""
import argparse
import json
import shutil
import sys
import urllib.request
from pathlib import Path

VERIFIED = "verified"
MISMATCH = "mismatch"
UNVERIFIABLE = "unverifiable"


class Case:
    def __init__(self, name, narrative, tables, expect=None, counts=None,
                 detail_contains=None, forbid=None):
        self.name = name
        self.narrative = narrative
        self.tables = tables  # {filename: csv/tsv text}
        self.expect = expect or {}  # {entity: status} subset assertions
        self.counts = counts  # (v, m, u) exact, or None to skip
        self.detail_contains = detail_contains or {}  # {entity: substring}
        self.forbid = forbid or []  # entities that must NOT be extracted


def post_verify(base, session, task):
    url = f"{base}/api/chat/session/{session}/task/{task}/verify"
    req = urllib.request.Request(url, method="POST", data=b"")
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def write_case(pkg: Path, task: str, case: Case):
    ndir = pkg / "runtime" / "outputs" / task
    tdir = pkg / "results" / "tables"
    if ndir.exists():
        shutil.rmtree(ndir)
    ndir.mkdir(parents=True)
    if tdir.exists():
        shutil.rmtree(tdir)
    tdir.mkdir(parents=True)
    (ndir / "report.md").write_text(case.narrative)
    for fname, body in case.tables.items():
        (tdir / fname).write_text(body)


def check(case: Case, report):
    """Return (ok, messages)."""
    msgs = []
    ok = True
    verdicts = report.get("verdicts", [])
    by_entity = {}
    for v in verdicts:
        by_entity.setdefault(v["claim"]["entity"], []).append(v)
    # per-entity expectations
    for entity, want in case.expect.items():
        got = by_entity.get(entity)
        if not got:
            ok = False
            msgs.append(f"  MISSING verdict for entity '{entity}' "
                        f"(extracted: {sorted(by_entity)})")
            continue
        statuses = [g["status"]["status"] for g in got]
        if want not in statuses:
            ok = False
            msgs.append(f"  entity '{entity}': want {want}, got {statuses}")
        # detail substring check
        if entity in case.detail_contains:
            sub = case.detail_contains[entity]
            details = []
            for g in got:
                st = g["status"]
                details.append(st.get("detail") or st.get("reason") or "")
            if not any(sub.lower() in d.lower() for d in details):
                ok = False
                msgs.append(f"  entity '{entity}': detail missing '{sub}' "
                            f"(details: {details})")
    # forbidden entities (common stats/method acronyms must not be claims)
    for entity in case.forbid:
        if entity in by_entity:
            ok = False
            msgs.append(f"  FORBIDDEN entity '{entity}' was extracted as a claim")
    # exact counts
    if case.counts is not None:
        want = {"n_verified": case.counts[0], "n_mismatch": case.counts[1],
                "n_unverifiable": case.counts[2]}
        for k, wv in want.items():
            if report.get(k) != wv:
                ok = False
                msgs.append(f"  count {k}: want {wv}, got {report.get(k)}")
    return ok, msgs


# ── DE table fixtures ────────────────────────────────────────────────────
DE = (
    "gene\tlog2FC\tpadj\n"
    "TP53\t3.20\t0.0001\n"
    "BRCA1\t-2.10\t0.002\n"
    "EGFR\t1.05\t0.04\n"
    "MYC\t-0.80\t0.30\n"      # not significant
    "AKT1\t0.50\t0.01\n"
)


def build_cases():
    cases = []

    # ── A. Sign / direction fabrication ──────────────────────────────────
    cases.append(Case(
        "A1_sign_flip_effectsize",
        # Large opposite value: magnitude check fires first (correct).
        "TP53 was downregulated (log2FC=-3.20) in treated cells (Table de).",
        {"de.tsv": DE},
        expect={"TP53": MISMATCH},
        detail_contains={"TP53": "effect size"},
    ))
    cases.append(Case(
        "A1b_pure_sign_branch",  # within magnitude tol but opposite sign
        "QQQ1 was upregulated (log2FC=0.02) (Table de).",
        {"de.tsv": "gene\tlog2FC\tpadj\nQQQ1\t-0.02\t0.001\n"},
        expect={"QQQ1": MISMATCH},
        detail_contains={"QQQ1": "sign"},
    ))
    cases.append(Case(
        "A2_direction_word_contradicts_table",
        "BRCA1 was clearly upregulated in the treated group (Table de).",
        {"de.tsv": DE},
        expect={"BRCA1": MISMATCH},
        detail_contains={"BRCA1": "direction"},
    ))
    cases.append(Case(
        "A3_correct_direction_no_number",
        "TP53 was strongly induced relative to control (Table de).",
        {"de.tsv": DE},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "A4_down_synonym_correct",
        "BRCA1 was markedly suppressed in the treated condition (Table de).",
        {"de.tsv": DE},
        expect={"BRCA1": VERIFIED},
    ))

    # ── B. Magnitude fabrication + tolerance boundaries ──────────────────
    cases.append(Case(
        "B1_magnitude_inflated",
        "TP53 showed a large effect (log2FC=8.00, padj=0.0001) (Table de).",
        {"de.tsv": DE},
        expect={"TP53": MISMATCH},
        detail_contains={"TP53": "effect size"},
    ))
    cases.append(Case(
        "B2_within_tolerance",  # 3.20 vs 3.24, tol 0.05
        "TP53 was upregulated (log2FC=3.24, padj=0.0001) (Table de).",
        {"de.tsv": DE},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "B3_just_over_tolerance",  # 3.20 vs 3.27, delta 0.07 > 0.05
        "TP53 was upregulated (log2FC=3.27, padj=0.0001) (Table de).",
        {"de.tsv": DE},
        expect={"TP53": MISMATCH},
    ))
    cases.append(Case(
        "B4_exact_match",
        "EGFR was upregulated (log2FC=1.05, padj=0.04) (Table de).",
        {"de.tsv": DE},
        expect={"EGFR": VERIFIED},
    ))

    # ── C. P-value fabrication + tolerance ───────────────────────────────
    cases.append(Case(
        "C1_pvalue_fabricated_small",
        "TP53 was upregulated (log2FC=3.20, padj=1e-12) (Table de).",
        {"de.tsv": DE},
        expect={"TP53": MISMATCH},
        detail_contains={"TP53": "p-value"},
    ))
    cases.append(Case(
        "C2_pvalue_within_rel_tol",  # 0.0001 vs 0.000105 (5% < 10%)
        "TP53 was upregulated (log2FC=3.20, padj=0.000105) (Table de).",
        {"de.tsv": DE},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "C3_scientific_notation_match",
        "TP53 was upregulated (log2FC=3.20, padj=1.0e-4) (Table de).",
        {"de.tsv": DE},
        expect={"TP53": VERIFIED},
    ))

    # ── D. Thresholded (FDR<0.05) ────────────────────────────────────────
    cases.append(Case(
        "D1_threshold_violation",  # MYC padj=0.30, claim significant
        "MYC was significantly downregulated at FDR < 0.05 (Table de).",
        {"de.tsv": DE},
        expect={"MYC": MISMATCH},
        detail_contains={"MYC": "FDR"},
    ))
    cases.append(Case(
        "D2_threshold_satisfied",
        "AKT1 was significantly upregulated at FDR < 0.05 (Table de).",
        {"de.tsv": DE},
        expect={"AKT1": VERIFIED},
    ))

    # ── E. Fabricated / missing entities ─────────────────────────────────
    cases.append(Case(
        "E1_fabricated_gene",
        "FAKEGENE99 was upregulated (log2FC=4.0) in treated cells (Table de).",
        {"de.tsv": DE},
        expect={"FAKEGENE99": UNVERIFIABLE},
        detail_contains={"FAKEGENE99": "not found"},
    ))
    cases.append(Case(
        "E2_no_table_cited",
        "TP53 was strongly upregulated (log2FC=3.20) in the treated group.",
        {"de.tsv": DE},
        expect={"TP53": UNVERIFIABLE},
    ))

    # ── F. Rank / top-N membership ───────────────────────────────────────
    # Table pre-sorted by importance; rank uses row order truncated to N.
    RANK = (
        "gene\tlog2FC\tpadj\n"
        "GENE1\t5.0\t0.001\n"
        "GENE2\t4.0\t0.001\n"
        "GENE3\t3.0\t0.001\n"
        "GENE4\t2.0\t0.001\n"
        "GENE5\t1.0\t0.001\n"
        "GENE6\t0.5\t0.001\n"
    )
    cases.append(Case(
        "F1_in_top_3",
        "GENE2 was among the top-3 ranked differentially expressed genes (Table rank).",
        {"rank.tsv": RANK},
        expect={"GENE2": VERIFIED},
    ))
    cases.append(Case(
        "F2_not_in_top_3",
        "GENE6 was among the top-3 ranked differentially expressed genes (Table rank).",
        {"rank.tsv": RANK},
        expect={"GENE6": MISMATCH},
        detail_contains={"GENE6": "top-3"},
    ))

    # ── G. Mixed multi-claim narrative ───────────────────────────────────
    cases.append(Case(
        "G1_mixed_verdicts",
        ("TP53 was upregulated (log2FC=3.20) and BRCA1 was downregulated "
         "(log2FC=-2.10) (Table de). FAKEX was upregulated (log2FC=9.0) (Table de). "
         "EGFR was downregulated (log2FC=-1.05) (Table de)."),
        {"de.tsv": DE},
        expect={"TP53": VERIFIED, "BRCA1": VERIFIED,
                "FAKEX": UNVERIFIABLE, "EGFR": MISMATCH},
    ))

    # ── H. Ambiguous table reference → Unverifiable ──────────────────────
    cases.append(Case(
        "H1_ambiguous_table_ref",
        "TP53 was upregulated (log2FC=3.20) (Table de).",
        {"de_a.tsv": DE, "de_b.tsv": DE},  # token 'de' matches both
        expect={"TP53": UNVERIFIABLE},
    ))

    # ── I. Edge cases ────────────────────────────────────────────────────
    cases.append(Case(
        "I1_exclude_patterns_not_claims",
        "DNA and RNA samples were processed; the USA cohort used PCR (Table de).",
        {"de.tsv": DE},
        counts=(0, 0, 0),  # excluded acronyms must not become claims
    ))
    cases.append(Case(
        "I2_case_insensitive_entity",  # narrative tp53 lower? entity pattern is uppercase
        "TP53 was upregulated (log2FC=3.20) (Table de).",
        {"de.tsv": "Gene\tLOG2FC\tPADJ\nTP53\t3.20\t0.0001\n"},  # caps headers
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "I3_csv_comma_separated",
        "TP53 was upregulated (log2FC=3.20, padj=0.0001) (Table de).",
        {"de.csv": "gene,log2FC,padj\nTP53,3.20,0.0001\n"},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "I4_effect_size_zero_table",  # obs 0.0, claimed up -> sign rule skips zero
        "ZZZ1 was upregulated (log2FC=0.00) (Table de).",
        {"de.tsv": "gene\tlog2FC\tpadj\nZZZ1\t0.00\t0.5\n"},
        expect={"ZZZ1": VERIFIED},
    ))
    cases.append(Case(
        "I5_alt_pvalue_column_FDR",
        "TP53 was upregulated (log2FC=3.20, padj=0.0001) (Table de).",
        {"de.tsv": "gene\tlog2FC\tFDR\nTP53\t3.20\t0.0001\n"},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "I6_alt_effect_column_logFC",
        "TP53 was upregulated (logFC=3.20) (Table de).",
        {"de.tsv": "gene\tlogFC\tpadj\nTP53\t3.20\t0.0001\n"},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "I7_alt_entity_column_symbol",
        "TP53 was upregulated (log2FC=3.20) (Table de).",
        {"de.tsv": "symbol\tlog2FC\tpadj\nTP53\t3.20\t0.0001\n"},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "I8_GO_term_entity",
        "GO:0006955 was enriched among upregulated genes (Table de).",
        {"de.tsv": "gene\tlog2FC\tpadj\nGO:0006955\t2.0\t0.001\n"},
        expect={"GO:0006955": VERIFIED},
    ))
    cases.append(Case(
        "I9_empty_table",
        "TP53 was upregulated (log2FC=3.20) (Table de).",
        {"de.tsv": "gene\tlog2FC\tpadj\n"},
        expect={"TP53": UNVERIFIABLE},
    ))
    cases.append(Case(
        "I10_table_missing_effect_col",  # has entity but no effect col -> unverifiable for effect
        "TP53 was upregulated (log2FC=3.20) (Table de).",
        {"de.tsv": "gene\tbaseMean\tpadj\nTP53\t100\t0.0001\n"},
        expect={"TP53": UNVERIFIABLE},
    ))

    # ══ Wave 2: deeper pathological / edge coverage ══════════════════════
    cases.append(Case(
        "W1_three_entities_three_numbers",  # nearest-binding stress
        ("TP53 was upregulated (log2FC=3.20), BRCA1 was downregulated "
         "(log2FC=-2.10), and EGFR was upregulated (log2FC=1.05) (Table de)."),
        {"de.tsv": DE},
        expect={"TP53": VERIFIED, "BRCA1": VERIFIED, "EGFR": VERIFIED},
    ))
    cases.append(Case(
        "W2_value_before_entity",  # single value preceding the entity
        "With a log2FC=3.20, TP53 was strongly upregulated (Table de).",
        {"de.tsv": DE},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "W3_pvalue_zero_claim_vs_nonzero",  # fabricated p=0
        "TP53 was upregulated (log2FC=3.20, padj=0.000) (Table de).",
        {"de.tsv": DE},
        expect={"TP53": MISMATCH},
        detail_contains={"TP53": "p-value"},
    ))
    cases.append(Case(
        "W4_pvalue_just_over_rel_tol",  # 0.0002 vs 0.0001 -> ratio ln2 >> ln1.1
        "TP53 was upregulated (log2FC=3.20, padj=0.0002) (Table de).",
        {"de.tsv": DE},
        expect={"TP53": MISMATCH},
        detail_contains={"TP53": "p-value"},
    ))
    cases.append(Case(
        "W5_duplicate_entity_first_row_wins",
        "TP53 was upregulated (log2FC=3.20) (Table de).",
        {"de.tsv": "gene\tlog2FC\tpadj\nTP53\t3.20\t0.001\nTP53\t9.90\t0.001\n"},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "W6_ragged_row_resilience",  # flexible CSV; good row still verifies
        "TP53 was upregulated (log2FC=3.20) (Table de).",
        {"de.tsv": "gene\tlog2FC\tpadj\nTP53\t3.20\t0.001\nMYC\t-0.80\n"},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "W7_reactome_entity_pattern",
        "R-HSA-109581 was enriched among upregulated genes (log2FC=2.0) (Table de).",
        {"de.tsv": "gene\tlog2FC\tpadj\nR-HSA-109581\t2.0\t0.001\n"},
        expect={"R-HSA-109581": VERIFIED},
    ))
    cases.append(Case(
        "W8_both_directions_nearest_wins",  # no numbers, direction-only
        "TP53 was upregulated while BRCA1 was downregulated (Table de).",
        {"de.tsv": DE},
        expect={"TP53": VERIFIED, "BRCA1": VERIFIED},
    ))
    cases.append(Case(
        "W9_down_synonym_repressed",
        "BRCA1 was strongly repressed in treated cells (Table de).",
        {"de.tsv": DE},
        expect={"BRCA1": VERIFIED},
    ))
    cases.append(Case(
        "W10_bare_filename_not_a_table_ref",  # documents: needs 'Table' keyword
        "TP53 was upregulated (log2FC=3.20) (see de.tsv).",
        {"de.tsv": DE},
        expect={"TP53": UNVERIFIABLE},
    ))
    cases.append(Case(
        "W11_categorical_label_match",
        "TP53 was classified as cluster Tumor in the annotation (Table cl).",
        {"cl.tsv": "gene\tcluster\nTP53\tTumor\n"},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "W12_categorical_label_mismatch",
        "TP53 was classified as cluster Tumor in the annotation (Table cl).",
        {"cl.tsv": "gene\tcluster\nTP53\tNormal\n"},
        expect={"TP53": MISMATCH},
    ))
    cases.append(Case(
        "W13_timeseries_coord_match",
        "TP53 expression peaked at day 7 of treatment (Table ts).",
        {"ts.tsv": "gene\tday\tlog2FC\nTP53\t7\t2.0\n"},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "W14_timeseries_coord_mismatch",
        "TP53 expression peaked at day 7 of treatment (Table ts).",
        {"ts.tsv": "gene\tday\tlog2FC\nTP53\t14\t2.0\n"},
        expect={"TP53": MISMATCH},
        detail_contains={"TP53": "time"},
    ))
    cases.append(Case(
        "W15_multiple_tables_disambiguated",
        "GO:0006955 was enriched (log2FC=2.0) (Table enrich).",
        {"de.tsv": DE,
         "enrich.tsv": "gene\tlog2FC\tpadj\nGO:0006955\t2.0\t0.001\n"},
        expect={"GO:0006955": VERIFIED},
    ))
    cases.append(Case(
        "W16_newline_separated_claims",
        ("TP53 was upregulated (log2FC=3.20) (Table de)\n"
         "BRCA1 was downregulated (log2FC=-2.10) (Table de)"),
        {"de.tsv": DE},
        expect={"TP53": VERIFIED, "BRCA1": VERIFIED},
    ))
    cases.append(Case(
        "W17_effect_scientific_notation",
        "TP53 was upregulated (log2FC=3.2e0) (Table de).",
        {"de.tsv": DE},
        expect={"TP53": VERIFIED},
    ))
    cases.append(Case(
        "W18_pvalue_zero_both_exact",
        "ZIP1 was upregulated (log2FC=2.0, padj=0) (Table z).",
        {"z.tsv": "gene\tlog2FC\tpadj\nZIP1\t2.0\t0\n"},
        expect={"ZIP1": VERIFIED},
    ))
    cases.append(Case(
        "W19_tolerance_inside_verified",  # delta 0.03 < tol 0.05
        "EGFR was upregulated (log2FC=1.08, padj=0.04) (Table de).",  # table 1.05
        {"de.tsv": DE},
        expect={"EGFR": VERIFIED},
    ))
    cases.append(Case(
        "W19b_tolerance_at_exact_boundary_mismatch",  # 1.05 vs 1.10 = 0.05+eps
        "EGFR was upregulated (log2FC=1.10, padj=0.04) (Table de).",
        {"de.tsv": DE},
        expect={"EGFR": MISMATCH},
    ))
    cases.append(Case(
        "W20_nan_pvalue_in_table_unverifiable",
        "TP53 was upregulated (log2FC=3.20, padj=0.001) (Table de).",
        {"de.tsv": "gene\tlog2FC\tpadj\nTP53\t3.20\tNaN\n"},
        expect={"TP53": UNVERIFIABLE},
    ))

    # ── Stress / scale ───────────────────────────────────────────────────
    big_rows = ["gene\tlog2FC\tpadj"]
    for i in range(5000):
        big_rows.append(f"GBIG{i}\t{(i % 7) - 3}.0\t0.001")
    big_table = "\n".join(big_rows) + "\n"
    cases.append(Case(
        "W21_large_table_5000rows",
        "GBIG4999 was downregulated (log2FC=-2.0) (Table big).",  # 4999%7=3 ->0 -> wait
        {"big.tsv": big_table},
        # 4999 % 7 = 3 -> 3-3 = 0.0; choose a gene with negative value instead
        expect={},  # replaced below
    ))
    # fix: pick a gene whose value is negative deterministically (i%7-3<0 => i%7 in {0,1,2})
    cases[-1].narrative = "GBIG2 was downregulated (log2FC=-1.0) (Table big)."  # 2%7-3=-1
    cases[-1].expect = {"GBIG2": VERIFIED}

    # ── Wave 3: false-negative probes (fabrications must NOT pass) ───────
    cases.append(Case(
        "X1_internal_contradiction_effect_vs_direction",
        # effect matches table (+3.20) but direction word says "down"
        "TP53 was downregulated (log2FC=3.20) in treated cells (Table de).",
        {"de.tsv": DE},
        expect={"TP53": MISMATCH},
        detail_contains={"TP53": "direction"},
    ))
    cases.append(Case(
        "X2_pvalue_underflow_both_match",
        "ZQ1 was upregulated (log2FC=2.0, padj=1e-300) (Table z).",
        {"z.tsv": "gene\tlog2FC\tpadj\nZQ1\t2.0\t1e-300\n"},
        expect={"ZQ1": VERIFIED},
    ))
    cases.append(Case(
        "X3_huge_fabricated_effect_matching_p",
        "TP53 was upregulated (log2FC=99.9, padj=0.0001) (Table de).",
        {"de.tsv": DE},
        expect={"TP53": MISMATCH},
        detail_contains={"TP53": "effect size"},
    ))
    cases.append(Case(
        "X4_fabrication_hidden_among_correct",
        ("TP53 was upregulated (log2FC=3.20), AKT1 was upregulated "
         "(log2FC=0.50), EGFR was upregulated (log2FC=1.05), and MYC was "
         "strongly upregulated (log2FC=4.40) (Table de)."),  # MYC table is -0.80
        {"de.tsv": DE},
        expect={"TP53": VERIFIED, "AKT1": VERIFIED, "EGFR": VERIFIED,
                "MYC": MISMATCH},
    ))
    cases.append(Case(
        "X5_cross_entity_value_not_borrowed",
        # BRCA1 cited with TP53's value; must compare to BRCA1's own row
        "BRCA1 was upregulated (log2FC=3.20) (Table de).",  # BRCA1 table is -2.10
        {"de.tsv": DE},
        expect={"BRCA1": MISMATCH},
    ))
    cases.append(Case(
        "X6_scientific_notation_effect_mismatch",
        "TP53 was upregulated (log2FC=1.0e1) (Table de).",  # 10.0 vs table 3.20
        {"de.tsv": DE},
        expect={"TP53": MISMATCH},
    ))

    many = ["gene\tlog2FC\tpadj"]
    narr = []
    for i in range(40):
        v = 2.0 + i * 0.01
        many.append(f"GM{i}\t{v:.2f}\t0.001")
        narr.append(f"GM{i} was upregulated (log2FC={v:.2f}) (Table many).")
    cases.append(Case(
        "W23_stats_acronyms_not_entities",
        ("AKT1 was significantly upregulated at FDR < 0.05 using GSEA on "
         "TPM-normalized counts; CPM and FPKM were also computed (Table de)."),
        {"de.tsv": DE},
        expect={"AKT1": VERIFIED},
        forbid=["FDR", "GSEA", "TPM", "CPM", "FPKM"],
    ))
    cases.append(Case(
        "W24_de_fc_deg_acronyms_not_entities",
        ("Across the DE analysis, 412 DEGs passed the FC threshold; the top "
         "DEG was TP53 (log2FC=3.20) (Table de)."),
        {"de.tsv": DE},
        expect={"TP53": VERIFIED},
        forbid=["DE", "DEG", "DEGS", "FC"],
    ))
    cases.append(Case(
        "W22_forty_claims_all_verified",
        " ".join(narr),
        {"many.tsv": "\n".join(many) + "\n"},
        counts=(40, 0, 0),
    ))
    return cases


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--session", required=True)
    ap.add_argument("--package", required=True)
    ap.add_argument("--task", default="reporting")
    ap.add_argument("--base", default="http://localhost:3000")
    ap.add_argument("--only", default=None, help="substring filter on case name")
    args = ap.parse_args()

    pkg = Path(args.package)
    cases = build_cases()
    if args.only:
        cases = [c for c in cases if args.only in c.name]

    n_pass = n_fail = 0
    failures = []
    for case in cases:
        write_case(pkg, args.task, case)
        try:
            resp = post_verify(args.base, args.session, args.task)
        except Exception as e:
            print(f"[FAIL] {case.name}: HTTP error {e}")
            n_fail += 1
            failures.append(case.name)
            continue
        report = resp.get("report")
        if report is None:
            print(f"[FAIL] {case.name}: null report ({resp.get('reason')})")
            n_fail += 1
            failures.append(case.name)
            continue
        ok, msgs = check(case, report)
        if ok:
            print(f"[PASS] {case.name}  "
                  f"(v={report['n_verified']} m={report['n_mismatch']} "
                  f"u={report['n_unverifiable']})")
            n_pass += 1
        else:
            print(f"[FAIL] {case.name}")
            for m in msgs:
                print(m)
            # dump extracted entities for debugging
            ents = [(v['claim']['entity'], v['status']['status'])
                    for v in report.get('verdicts', [])]
            print(f"  extracted: {ents}")
            n_fail += 1
            failures.append(case.name)

    print(f"\n=== {n_pass} passed, {n_fail} failed (of {len(cases)}) ===")
    if failures:
        print("FAILURES:", ", ".join(failures))
    sys.exit(1 if n_fail else 0)


if __name__ == "__main__":
    main()
