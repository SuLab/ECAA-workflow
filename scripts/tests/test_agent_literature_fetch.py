"""Tests for the agent literature-retrieval helper.

Runs under pytest (`python3 -m pytest`) but uses only stdlib `unittest`
assertions where possible so a bare `python3 -m unittest` also works.

The network layer (`_http_get_json` / `_http_get_text`) is the single
monkeypatchable seam: every test stubs it so no real egress happens.
"""

import csv
import hashlib
import json
import os
import sys
import unittest
from pathlib import Path

# Import the helper that lives one directory up (scripts/).
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import agent_literature_fetch as alf  # noqa: E402


def _read_manifest(out: Path):
    return json.loads((out / "evidence" / "manifest.json").read_text())


def _read_csv_rows(out: Path):
    text = (out / "method_landscape.csv").read_text()
    return list(csv.DictReader(text.splitlines()))


def _read_retrieval_scope(out: Path):
    return json.loads((out / "retrieval_scope.json").read_text())


class NormalizerTest(unittest.TestCase):
    def test_collapse_whitespace_lowercase_v1(self):
        self.assertEqual(
            alf.normalize_text("  STAR   Aligns\n\tReads "),
            "star aligns reads",
        )


class HostGuardTest(unittest.TestCase):
    def test_non_allowlisted_host_raises(self):
        with self.assertRaises(alf.HostNotAllowedError):
            alf._http_get_json(
                "https://evil.example.com/x", "evil.example.com", ["api.openalex.org"]
            )

    def test_non_allowlisted_text_host_raises(self):
        with self.assertRaises(alf.HostNotAllowedError):
            alf._http_get_text("https://evil.example.com/x", "evil.example.com", ["readthedocs.io"])


class OpenAlexFetchTest(unittest.TestCase):
    def test_openalex_fetch_writes_snapshot_and_manifest(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            captured = {}

            def fake_get_json(url, host, allowed_hosts):
                captured["host"] = host
                captured["allowed"] = allowed_hosts
                # host guard still exercised inside the real impl path; here
                # we replace the whole function, so assert manually.
                assert host in allowed_hosts, "stub must respect allowlist"
                return {
                    "results": [
                        {
                            "doi": "https://doi.org/10.1/x",
                            "display_name": "STAR aligner",
                            "abstract_inverted_index": {
                                "STAR": [0],
                                "aligns": [1],
                                "reads": [2],
                            },
                        }
                    ]
                }

            alf._http_get_json = fake_get_json
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="alignment",
                query="STAR RNA-seq aligner",
                classes=["conference_proceedings"],
                routes={"conference_proceedings": {"hosts": ["api.openalex.org"]}},
            )

            manifest = _read_manifest(out)
            self.assertEqual(manifest["schema_version"], 2)
            self.assertEqual(len(manifest["entries"]), 1)
            e = manifest["entries"][0]
            self.assertEqual(e["source_ref_kind"], "doi")
            self.assertEqual(e["source_class"], "conference_proceedings")
            self.assertEqual(e["source_kind"], "openalex")
            self.assertEqual(e["source_ref"], "10.1/x")
            # sha256_binary matches the snapshot bytes on disk.
            snap = (out / "evidence" / e["path"]).read_bytes()
            self.assertEqual(hashlib.sha256(snap).hexdigest(), e["sha256_binary"])
            # retrieval_query_id obeys the ^q[0-9]{3,}$ pattern.
            self.assertRegex(e["retrieval_query_id"], r"^q[0-9]{3,}$")

            rows = _read_csv_rows(out)
            self.assertEqual(len(rows), 1)
            r = rows[0]
            self.assertEqual(r["axis"], "alignment")
            self.assertEqual(r["candidate_method"], "STAR aligner")
            self.assertEqual(r["source_ref_kind"], "doi")
            self.assertEqual(r["source_ref"], "10.1/x")
            self.assertEqual(r["source_class"], "conference_proceedings")
            self.assertEqual(r["source_hash"], "sha256:" + e["sha256_binary"])
            # The evidence quote substring-matches the snapshot, so verified.
            self.assertEqual(r["verified"], "true")


class PrimaryLiteratureFetchTest(unittest.TestCase):
    """PubMed esearch→efetch must emit validator-passing per-PMID evidence:
    singular `pmid` on both the manifest entry and the CSV row,
    source_kind=pubmed_abstract, redistributable=true, verified=true."""

    def test_primary_literature_esearch_efetch_emits_per_pmid_evidence(self):
        import tempfile

        # esearch (JSON) returns two PMIDs; efetch (XML) returns one abstract
        # record per request. The verbatim evidence quote must substring-match
        # the extracted abstract text.
        def fake_get_json(url, host, allowed_hosts):
            assert "esearch" in url, f"primary-lit JSON call must be esearch: {url}"
            assert host in allowed_hosts
            return {"esearchresult": {"idlist": ["19029910", "30656827"]}}

        def fake_get_text(url, host, allowed_hosts):
            assert "efetch" in url, f"primary-lit text call must be efetch: {url}"
            assert host in allowed_hosts
            pmid = "19029910" if "19029910" in url else "30656827"
            title = "MaxQuant enables high peptide identification rates"
            return (
                '<?xml version="1.0"?><PubmedArticleSet><PubmedArticle>'
                f"<MedlineCitation><PMID>{pmid}</PMID><Article>"
                f"<ArticleTitle>{title}</ArticleTitle>"
                "<Abstract><AbstractText>MaxQuant enables high peptide "
                "identification rates, individualized ppb-range mass "
                "accuracies and proteome-wide protein quantification."
                "</AbstractText></Abstract>"
                "</Article></MedlineCitation></PubmedArticle></PubmedArticleSet>"
            )

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            alf._http_get_json = fake_get_json
            alf._http_get_text = fake_get_text
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="peptide_search",
                query="MaxQuant peptide identification",
                classes=["primary_literature"],
                routes={"primary_literature": {"hosts": ["eutils.ncbi.nlm.nih.gov"]}},
            )

            manifest = _read_manifest(out)
            self.assertEqual(manifest["schema_version"], 2)
            self.assertGreaterEqual(len(manifest["entries"]), 2)
            for e in manifest["entries"]:
                # run_pmid_resolves keys the manifest by SINGULAR pmid.
                self.assertIn("pmid", e)
                self.assertRegex(e["pmid"], r"^[1-9][0-9]{6,8}$")
                self.assertEqual(e["source_kind"], "pubmed_abstract")
                self.assertTrue(e["redistributable"])
                # snapshot file exists on disk and hashes match.
                snap = (out / "evidence" / e["path"]).read_bytes()
                self.assertEqual(hashlib.sha256(snap).hexdigest(), e["sha256_binary"])

            rows = _read_csv_rows(out)
            self.assertGreaterEqual(len(rows), 2)
            for r in rows:
                self.assertRegex(r["pmid"], r"^[1-9][0-9]{6,8}$")
                self.assertEqual(r["source_ref_kind"], "pmid")
                self.assertEqual(r["source_ref"], r["pmid"])
                self.assertEqual(r["source_kind"], "pubmed_abstract")
                self.assertEqual(r["redistributable"], "true")
                self.assertEqual(r["verified"], "true")

    def test_snapshot_stores_full_abstract_not_just_first_sentence(self):
        """ROOT-FIX faithful twin: the snapshot bytes (sha256_binary) must be
        the FULL abstract, not the ~topic-first-sentence evidence_quote. A
        multi-sentence abstract is stubbed; the stored snapshot must equal the
        whole abstract (and be strictly longer than the first sentence). A
        quote genuinely absent from the abstract must report not-present
        (verified=false) — the quote-presence check is real, not rubber-stamped.
        """
        import tempfile

        first_sentence = "MaxQuant enables high peptide identification rates."
        rest = (
            " It provides individualized ppb-range mass accuracies as a "
            "function of peptide mass and elution time, and proteome-wide "
            "protein quantification by delayed normalization and maximal "
            "peptide ratio extraction across many samples."
        )
        full_abstract = first_sentence + rest
        # Sanity: the helper's quote IS only the first sentence, so the full
        # abstract is strictly longer — this is the exact gap the fix closes.
        self.assertEqual(alf._pubmed_evidence_quote(full_abstract), first_sentence)
        self.assertGreater(len(full_abstract), len(first_sentence))

        def fake_get_json(url, host, allowed_hosts):
            assert "esearch" in url
            return {"esearchresult": {"idlist": ["19029910"]}}

        def fake_get_text(url, host, allowed_hosts):
            assert "efetch" in url
            return (
                '<?xml version="1.0"?><PubmedArticleSet><PubmedArticle>'
                "<MedlineCitation><PMID>19029910</PMID><Article>"
                "<ArticleTitle>MaxQuant</ArticleTitle>"
                f"<Abstract><AbstractText>{full_abstract}</AbstractText>"
                "</Abstract></Article></MedlineCitation>"
                "</PubmedArticle></PubmedArticleSet>"
            )

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            alf._http_get_json = fake_get_json
            alf._http_get_text = fake_get_text
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="peptide_search",
                query="MaxQuant peptide identification",
                classes=["primary_literature"],
                routes={"primary_literature": {"hosts": ["eutils.ncbi.nlm.nih.gov"]}},
            )

            manifest = _read_manifest(out)
            entries = [e for e in manifest["entries"] if e.get("pmid") == "19029910"]
            self.assertEqual(len(entries), 1)
            e = entries[0]
            snap = (out / "evidence" / e["path"]).read_bytes()
            # The snapshot bytes ARE the full abstract, not the topic sentence.
            self.assertEqual(snap.decode("utf-8"), full_abstract)
            self.assertGreater(len(snap), len(first_sentence.encode("utf-8")))
            self.assertEqual(hashlib.sha256(snap).hexdigest(), e["sha256_binary"])
            self.assertEqual(int(e["bytes"]), len(full_abstract.encode("utf-8")))

            # The verbatim first-sentence quote IS present in the full snapshot.
            rows = _read_csv_rows(out)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["verified"], "true")
            self.assertIn(rows[0]["evidence_quote"], full_abstract)

    def test_quote_absent_from_abstract_reports_not_present(self):
        """Faithful twin (negative): a finding whose verbatim quote does NOT
        occur in its snapshotted abstract must NOT be marked verified. Proves
        the quote-presence check still genuinely rejects a non-matching quote
        rather than rubber-stamping every row."""
        import tempfile

        abstract = (
            "Salmon quantifies transcript abundance from RNA-seq reads using "
            "a dual-phase inference procedure."
        )
        # Inject a finding whose quote is absent from the abstract by stubbing
        # the quote-picker for this one record.
        orig_quote = alf._pubmed_evidence_quote
        try:
            alf._pubmed_evidence_quote = lambda _a: "this phrase is not in the abstract"

            def fake_get_json(url, host, allowed_hosts):
                return {"esearchresult": {"idlist": ["30940177"]}}

            def fake_get_text(url, host, allowed_hosts):
                return (
                    '<?xml version="1.0"?><PubmedArticleSet><PubmedArticle>'
                    "<MedlineCitation><PMID>30940177</PMID><Article>"
                    "<ArticleTitle>Salmon</ArticleTitle>"
                    f"<Abstract><AbstractText>{abstract}</AbstractText>"
                    "</Abstract></Article></MedlineCitation>"
                    "</PubmedArticle></PubmedArticleSet>"
                )

            with tempfile.TemporaryDirectory() as tmp:
                out = Path(tmp) / "out"
                out.mkdir()
                alf._http_get_json = fake_get_json
                alf._http_get_text = fake_get_text
                alf.fetch_for_axis(
                    out_dir=str(out),
                    axis="quant",
                    query="Salmon RNA-seq quantification",
                    classes=["primary_literature"],
                    routes={"primary_literature": {"hosts": ["eutils.ncbi.nlm.nih.gov"]}},
                )
                rows = _read_csv_rows(out)
                self.assertEqual(len(rows), 1)
                # Quote absent from the (full) snapshot -> not present.
                self.assertEqual(rows[0]["verified"], "false")
                # ...but the snapshot still faithfully stores the full abstract.
                manifest = _read_manifest(out)
                e = next(x for x in manifest["entries"] if x.get("pmid") == "30940177")
                snap = (out / "evidence" / e["path"]).read_bytes()
                self.assertEqual(snap.decode("utf-8"), abstract)
        finally:
            alf._pubmed_evidence_quote = orig_quote

    def test_candidate_override_groups_pmids_under_one_method(self):
        """With an explicit candidate, every retrieved PMID is tagged with that
        method (not the paper title) so corroboration (≥2 distinct PMIDs per
        candidate) is satisfiable. The agent calls the helper per candidate."""
        import tempfile

        def fake_get_json(url, host, allowed_hosts):
            return {"esearchresult": {"idlist": ["19029910", "30656827"]}}

        def fake_get_text(url, host, allowed_hosts):
            pmid = "19029910" if "19029910" in url else "30656827"
            return (
                "<PubmedArticleSet><PubmedArticle><MedlineCitation>"
                f"<PMID>{pmid}</PMID><Article><ArticleTitle>Some paper {pmid}"
                "</ArticleTitle><Abstract><AbstractText>Regression modelling "
                "estimates covariate-adjusted associations between features and "
                "an outcome.</AbstractText></Abstract></Article>"
                "</MedlineCitation></PubmedArticle></PubmedArticleSet>"
            )

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            alf._http_get_json = fake_get_json
            alf._http_get_text = fake_get_text
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="generic_summary",
                query="regression modelling metabolomics blood pressure",
                classes=["primary_literature"],
                routes={"primary_literature": {"hosts": ["eutils.ncbi.nlm.nih.gov"]}},
                candidate="regression_modeling",
            )
            rows = _read_csv_rows(out)
            self.assertGreaterEqual(len(rows), 2)
            cands = {r["candidate_method"] for r in rows}
            self.assertEqual(cands, {"regression_modeling"})
            # ≥2 distinct verified PMIDs under the single candidate.
            pmids = {r["pmid"] for r in rows if r["verified"] == "true"}
            self.assertGreaterEqual(len(pmids), 2)

    def test_candidate_override_selects_method_naming_quote(self):
        import tempfile

        alf._http_get_json = lambda url, host, allowed: {"esearchresult": {"idlist": ["25217409"]}}
        abstract = (
            "RNA sequencing is widely used in transcriptomics. "
            "DESeq2 estimates sample-specific size factors and fits "
            "negative-binomial generalized linear models."
        )
        alf._http_get_text = lambda url, host, allowed: (
            "<PubmedArticleSet><PubmedArticle><MedlineCitation>"
            "<PMID>25217409</PMID><Article><ArticleTitle>DESeq2 methods"
            f"</ArticleTitle><Abstract><AbstractText>{abstract}</AbstractText>"
            "</Abstract></Article></MedlineCitation></PubmedArticle>"
            "</PubmedArticleSet>"
        )

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="normalisation",
                query="DESeq2 normalization RNA-seq",
                classes=["primary_literature"],
                routes={"primary_literature": {"hosts": ["eutils.ncbi.nlm.nih.gov"]}},
                curated=["deseq2_vst"],
                candidate="deseq2_vst",
            )
            rows = _read_csv_rows(out)
            self.assertEqual(len(rows), 1)
            self.assertIn("DESeq2", rows[0]["evidence_quote"])
            self.assertNotEqual(
                rows[0]["evidence_quote"],
                "RNA sequencing is widely used in transcriptomics.",
            )
            self.assertEqual(rows[0]["verified"], "true")

    def test_candidate_override_rejects_unrelated_query_hits(self):
        import tempfile

        alf._http_get_json = lambda url, host, allowed: {"esearchresult": {"idlist": ["25217409"]}}
        alf._http_get_text = lambda url, host, allowed: (
            "<PubmedArticleSet><PubmedArticle><MedlineCitation>"
            "<PMID>25217409</PMID><Article><ArticleTitle>Unrelated analysis"
            "</ArticleTitle><Abstract><AbstractText>"
            "A generic transcriptomics study compared two groups."
            "</AbstractText></Abstract></Article></MedlineCitation>"
            "</PubmedArticle></PubmedArticleSet>"
        )

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            summary = alf.fetch_for_axis(
                out_dir=str(out),
                axis="normalisation",
                query="DESeq2 normalization RNA-seq",
                classes=["primary_literature"],
                routes={"primary_literature": {"hosts": ["eutils.ncbi.nlm.nih.gov"]}},
                curated=["deseq2_vst", "edger_tmm"],
                candidate="deseq2_vst",
            )
            self.assertEqual(summary["candidate_mismatch_filtered"], 1)
            self.assertTrue(summary["fallback_used"])
            rows = _read_csv_rows(out)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["candidate_method"], "deseq2_vst")
            self.assertEqual(rows[0]["source_class"], "curated_baseline")
            self.assertEqual(_read_manifest(out)["entries"], [])

    def test_ambiguous_method_name_requires_canonical_case(self):
        self.assertEqual(
            alf.candidate_evidence_quote("Mast cells were quantified in airway tissue.", "mast"),
            "",
        )
        self.assertIn(
            "MAST",
            alf.candidate_evidence_quote(
                "MAST fits hurdle models to single-cell expression.", "mast"
            ),
        )


class CandidateQueryWideningTest(unittest.TestCase):
    @staticmethod
    def _query(url):
        from urllib.parse import parse_qs, urlparse

        return parse_qs(urlparse(url).query).get("term", [""])[0]

    @staticmethod
    def _pubmed_xml(url, method_name):
        from urllib.parse import parse_qs, urlparse

        pmid = parse_qs(urlparse(url).query)["id"][0]
        return (
            "<PubmedArticleSet><PubmedArticle><MedlineCitation>"
            f"<PMID>{pmid}</PMID><Article><ArticleTitle>"
            f"{method_name} study {pmid}</ArticleTitle><Abstract><AbstractText>"
            f"{method_name} was evaluated using benchmark {pmid}."
            "</AbstractText></Abstract></Article></MedlineCitation>"
            "</PubmedArticle></PubmedArticleSet>"
        )

    def test_narrow_query_widens_to_candidate_only_query(self):
        import tempfile

        calls = []
        original = "spectral partition sparse matrices with contextual constraints"

        def fake_get_json(url, host, allowed_hosts):
            query = self._query(url)
            calls.append(query)
            ids = ["70000001", "70000002"] if query == "spectral partition" else []
            return {"esearchresult": {"idlist": ids}}

        alf._http_get_json = fake_get_json
        alf._http_get_text = lambda url, host, allowed: self._pubmed_xml(url, "Spectral partition")

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            summary = alf.fetch_for_axis(
                out_dir=str(out),
                axis="generic_partitioning",
                query=original,
                classes=["primary_literature"],
                routes={"primary_literature": {"hosts": ["eutils.ncbi.nlm.nih.gov"]}},
                curated=["spectral_partition"],
                candidate="spectral_partition",
            )

            self.assertEqual(calls, [original, "spectral partition"])
            self.assertEqual(summary["queries_attempted"], calls)
            self.assertEqual(summary["verified_paper_sources"], 2)
            self.assertFalse(summary["fallback_used"])
            self.assertEqual(len(_read_csv_rows(out)), 2)
            self.assertEqual(len(_read_manifest(out)["entries"]), 2)
            scope_queries = {entry["query"] for entry in _read_retrieval_scope(out)["axes"]}
            self.assertEqual(scope_queries, set(calls))

    def test_sufficient_original_query_does_not_widen(self):
        import tempfile

        calls = []
        original = "spectral partition benchmark"

        def fake_get_json(url, host, allowed_hosts):
            calls.append(self._query(url))
            return {"esearchresult": {"idlist": ["70000011", "70000012"]}}

        alf._http_get_json = fake_get_json
        alf._http_get_text = lambda url, host, allowed: self._pubmed_xml(url, "Spectral partition")

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            summary = alf.fetch_for_axis(
                out_dir=str(out),
                axis="generic_partitioning",
                query=original,
                classes=["primary_literature"],
                candidate="spectral_partition",
            )

            self.assertEqual(calls, [original])
            self.assertEqual(summary["queries_attempted"], [original])
            self.assertEqual(len(_read_csv_rows(out)), 2)

    def test_sources_are_deduplicated_across_queries_and_reruns(self):
        import tempfile

        calls = []
        original = "adaptive solver constrained inputs"

        def fake_get_json(url, host, allowed_hosts):
            query = self._query(url)
            calls.append(query)
            ids = ["70000021"] if query == original else ["70000021", "70000022"]
            return {"esearchresult": {"idlist": ids}}

        alf._http_get_json = fake_get_json
        alf._http_get_text = lambda url, host, allowed: self._pubmed_xml(url, "Adaptive solver")

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            kwargs = {
                "out_dir": str(out),
                "axis": "generic_optimization",
                "query": original,
                "classes": ["primary_literature"],
                "candidate": "adaptive_solver",
            }
            first = alf.fetch_for_axis(**kwargs)
            second = alf.fetch_for_axis(**kwargs)

            self.assertEqual(first["verified_paper_sources"], 2)
            self.assertEqual(second["verified_paper_sources"], 2)
            self.assertEqual(len(_read_csv_rows(out)), 2)
            self.assertEqual(len(_read_manifest(out)["entries"]), 2)
            # First call widens once. The rerun needs only its declared query
            # because retained support already meets the packaged policy.
            self.assertEqual(calls, [original, "adaptive solver", original])

    def test_genuinely_thin_corpus_retains_one_source_without_fabrication(self):
        import tempfile

        calls = []
        original = "adaptive solver constrained inputs"

        def fake_get_json(url, host, allowed_hosts):
            calls.append(self._query(url))
            return {"esearchresult": {"idlist": ["70000031"]}}

        alf._http_get_json = fake_get_json
        alf._http_get_text = lambda url, host, allowed: self._pubmed_xml(url, "Adaptive solver")

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            summary = alf.fetch_for_axis(
                out_dir=str(out),
                axis="generic_optimization",
                query=original,
                classes=["primary_literature"],
                curated=["adaptive_solver"],
                candidate="adaptive_solver",
            )

            self.assertEqual(len(calls), alf.MAX_CANDIDATE_QUERY_ATTEMPTS)
            self.assertEqual(summary["verified_paper_sources"], 1)
            self.assertFalse(summary["fallback_used"])
            rows = _read_csv_rows(out)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["source_class"], "primary_literature")

    def test_package_policy_controls_widening_stop(self):
        import tempfile

        calls = []
        original = "spectral partition contextual benchmark"

        def fake_get_json(url, host, allowed_hosts):
            query = self._query(url)
            calls.append(query)
            ids = (
                ["70000041", "70000042"]
                if query == original
                else ["70000041", "70000042", "70000043"]
            )
            return {"esearchresult": {"idlist": ids}}

        alf._http_get_json = fake_get_json
        alf._http_get_text = lambda url, host, allowed: self._pubmed_xml(url, "Spectral partition")

        with tempfile.TemporaryDirectory() as tmp:
            package = Path(tmp) / "package"
            out = package / "runtime" / "outputs" / "survey"
            out.mkdir(parents=True)
            policies = package / "policies"
            policies.mkdir()
            (policies / "source-discovery-policy.json").write_text(
                json.dumps({"claimSupportRules": {"minimumIndependentSources": 3}}),
                encoding="utf-8",
            )
            summary = alf.fetch_for_axis(
                out_dir=str(out),
                axis="generic_partitioning",
                query=original,
                classes=["primary_literature"],
                candidate="spectral_partition",
            )

            self.assertEqual(summary["minimum_independent_sources"], 3)
            self.assertEqual(summary["verified_paper_sources"], 3)
            self.assertEqual(calls, [original, "spectral partition"])
            self.assertEqual(len(_read_csv_rows(out)), 3)

    def test_axis_level_call_preserves_single_query_behavior(self):
        import tempfile

        calls = []

        def fake_get_json(url, host, allowed_hosts):
            calls.append(self._query(url))
            return {"esearchresult": {"idlist": []}}

        alf._http_get_json = fake_get_json
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            summary = alf.fetch_for_axis(
                out_dir=str(out),
                axis="generic_landscape",
                query="context-rich landscape query",
                classes=["primary_literature"],
            )

            self.assertEqual(calls, ["context-rich landscape query"])
            self.assertEqual(summary["queries_attempted"], ["context-rich landscape query"])


class RetrievalScopeTest(unittest.TestCase):
    def test_zero_result_axis_is_retained_without_a_csv_row(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()

            def no_hits(url, host, allowed_hosts):
                return {"esearchresult": {"idlist": []}}

            alf._http_get_json = no_hits
            summary = alf.fetch_for_axis(
                out_dir=str(out),
                axis="SPDEF",
                query="SPDEF dexamethasone airway smooth muscle",
                classes=["primary_literature"],
                routes={"primary_literature": {"hosts": ["eutils.ncbi.nlm.nih.gov"]}},
            )

            self.assertEqual(summary["rows_written"], 0)
            self.assertEqual(_read_csv_rows(out), [])
            scope = _read_retrieval_scope(out)
            self.assertEqual(scope["schema_version"], 1)
            self.assertEqual(
                scope["axes"],
                [
                    {
                        "axis": "SPDEF",
                        "candidate_mismatch_filtered": 0,
                        "entries_written": 0,
                        "fallback_used": False,
                        "query": "SPDEF dexamethasone airway smooth muscle",
                        "rows_written": 0,
                        "status": "completed",
                        "truncated_at_storage_cap": False,
                    }
                ],
            )

    def test_malformed_existing_scope_is_not_silently_replaced(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            scope_path = out / "retrieval_scope.json"
            scope_path.write_text("{not-json", encoding="utf-8")

            with self.assertRaisesRegex(RuntimeError, "malformed retrieval scope"):
                alf._record_retrieval_axis(
                    out,
                    "SPDEF",
                    "SPDEF dexamethasone airway smooth muscle",
                    status="attempted",
                )
            self.assertEqual(scope_path.read_text(encoding="utf-8"), "{not-json")

    def test_distinct_queries_under_one_analysis_axis_are_not_overwritten(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()

            alf._record_retrieval_axis(
                out,
                "differential_expression",
                "DESeq2 negative binomial differential expression",
                status="attempted",
            )
            alf._record_retrieval_axis(
                out,
                "differential_expression",
                "DESeq2 negative binomial differential expression",
                status="completed",
                rows_written=2,
            )
            alf._record_retrieval_axis(
                out,
                "differential_expression",
                "limma voom precision weights",
                status="attempted",
            )
            alf._record_retrieval_axis(
                out,
                "differential_expression",
                "limma voom precision weights",
                status="completed",
                rows_written=0,
            )

            self.assertEqual(
                _read_retrieval_scope(out)["axes"],
                [
                    {
                        "axis": "differential_expression",
                        "query": "DESeq2 negative binomial differential expression",
                        "rows_written": 2,
                        "status": "completed",
                    },
                    {
                        "axis": "differential_expression",
                        "query": "limma voom precision weights",
                        "rows_written": 0,
                        "status": "completed",
                    },
                ],
            )


class RateLimitRetryTest(unittest.TestCase):
    """NCBI E-utilities rate-limit (3 req/s no key, 10 with key). _raw_get must
    pace requests and retry 429/503 with backoff so a transient throttle does
    not fail the whole axis to the curated fallback (the real in-container
    blocker: a burst of esearch+efetch → instant HTTP 429)."""

    def setUp(self):
        self._orig_urlopen = alf.urlopen
        self._orig_interval = alf._MIN_REQUEST_INTERVAL
        alf._MIN_REQUEST_INTERVAL = 0.0  # keep the test fast

    def tearDown(self):
        alf.urlopen = self._orig_urlopen
        alf._MIN_REQUEST_INTERVAL = self._orig_interval

    def test_raw_get_retries_on_429_then_succeeds(self):
        import urllib.error

        calls = {"n": 0}

        class FakeResp:
            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False

            def read(self):
                return b"OK-BODY"

        def fake_urlopen(req, timeout=None):
            calls["n"] += 1
            if calls["n"] == 1:
                raise urllib.error.HTTPError(
                    "https://eutils.ncbi.nlm.nih.gov/x",
                    429,
                    "Too Many Requests",
                    {"Retry-After": "0"},
                    None,
                )
            return FakeResp()

        alf.urlopen = fake_urlopen
        out = alf._raw_get("https://eutils.ncbi.nlm.nih.gov/x")
        self.assertEqual(out, b"OK-BODY")
        self.assertEqual(calls["n"], 2, "must retry once after a 429")

    def test_raw_get_gives_up_after_max_retries(self):
        import urllib.error

        def always_429(req, timeout=None):
            raise urllib.error.HTTPError(
                "https://eutils.ncbi.nlm.nih.gov/x",
                429,
                "Too Many Requests",
                {"Retry-After": "0"},
                None,
            )

        alf.urlopen = always_429
        with self.assertRaises(urllib.error.HTTPError):
            alf._raw_get("https://eutils.ncbi.nlm.nih.gov/x")


class ToolDocFetchTest(unittest.TestCase):
    def test_tool_doc_page_yields_url_entry_with_version_context(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()

            html = (
                "<html><body><h1>FLAIR documentation</h1>"
                "<p>FLAIR v2 detects isoforms from long reads.</p>"
                "</body></html>"
            )

            def fake_get_text(url, host, allowed_hosts):
                assert host in allowed_hosts
                return html

            alf._http_get_text = fake_get_text
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="isoform_detection",
                query="FLAIR",
                classes=["tool_documentation"],
                routes={
                    "tool_documentation": {
                        "hosts": ["flair.readthedocs.io"],
                        "doc_urls": ["https://flair.readthedocs.io/en/latest/"],
                        "candidate": "FLAIR",
                    }
                },
            )

            manifest = _read_manifest(out)
            e = manifest["entries"][0]
            self.assertEqual(e["source_ref_kind"], "url")
            self.assertEqual(e["source_class"], "tool_documentation")
            self.assertEqual(e["source_kind"], "doc_page")
            self.assertEqual(e["evidence_role"], "capability_or_version")
            self.assertEqual(e["version_context"], "2")
            snap = (out / "evidence" / e["path"]).read_bytes()
            self.assertEqual(hashlib.sha256(snap).hexdigest(), e["sha256_binary"])

            rows = _read_csv_rows(out)
            r = rows[0]
            self.assertEqual(r["source_class"], "tool_documentation")
            self.assertEqual(r["evidence_role"], "capability_or_version")
            self.assertEqual(r["version_context"], "2")
            self.assertEqual(r["verified"], "true")

    def test_non_allowlisted_doc_host_raises(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            # Do NOT stub _http_get_text: the real impl must raise on a
            # host that is not in the route allowlist.
            import importlib

            importlib.reload(alf)
            with self.assertRaises(alf.HostNotAllowedError):
                alf.fetch_for_axis(
                    out_dir=str(out),
                    axis="isoform_detection",
                    query="FLAIR",
                    classes=["tool_documentation"],
                    routes={
                        "tool_documentation": {
                            "hosts": ["flair.readthedocs.io"],
                            # URL host (evil.example.com) is NOT in hosts.
                            "doc_urls": ["https://evil.example.com/flair"],
                            "candidate": "FLAIR",
                        }
                    },
                )


class ManifestSchemaConformanceTest(unittest.TestCase):
    def test_emitted_manifest_validates_against_foundation_v2_schema(self):
        try:
            import jsonschema  # type: ignore
        except Exception:  # pragma: no cover - host without jsonschema
            self.skipTest("jsonschema not installed")
        import tempfile

        schema_path = (
            Path(__file__).resolve().parents[2]
            / "config"
            / "stage-atoms"
            / "schemas"
            / "literature_evidence_manifest.schema.json"
        )
        if not schema_path.exists():
            self.skipTest("evidence-manifest schema not present in this checkout")
        schema = json.loads(schema_path.read_text())

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            alf._http_get_json = lambda url, host, allowed: {
                "results": [
                    {
                        "doi": "https://doi.org/10.1/x",
                        "display_name": "STAR aligner",
                        "abstract_inverted_index": {"STAR": [0], "aligns": [1]},
                    }
                ]
            }
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="alignment",
                query="STAR",
                classes=["conference_proceedings"],
                routes={"conference_proceedings": {"hosts": ["api.openalex.org"]}},
            )
            alf._http_get_text = lambda u, h, a: "<html><p>FLAIR v2 detects isoforms.</p></html>"
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="iso",
                query="FLAIR",
                classes=["tool_documentation"],
                routes={
                    "tool_documentation": {
                        "hosts": ["flair.readthedocs.io"],
                        "doc_urls": ["https://flair.readthedocs.io/x"],
                        "candidate": "FLAIR",
                    }
                },
            )
            manifest = _read_manifest(out)
            jsonschema.validate(manifest, schema)

    def test_primary_and_fallback_rows_validate_against_matrix_schema(self):
        try:
            import jsonschema  # type: ignore
        except Exception:  # pragma: no cover - host without jsonschema
            self.skipTest("jsonschema not installed")
        import tempfile

        schema_path = (
            Path(__file__).resolve().parents[2]
            / "config"
            / "stage-atoms"
            / "schemas"
            / "method_landscape_matrix.schema.json"
        )
        schema = json.loads(schema_path.read_text())

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            alf._http_get_json = lambda url, host, allowed: {
                "esearchresult": {"idlist": ["25217409"]}
            }
            alf._http_get_text = lambda url, host, allowed: (
                "<PubmedArticleSet><PubmedArticle><MedlineCitation>"
                "<PMID>25217409</PMID><Article><ArticleTitle>DESeq2 methods"
                "</ArticleTitle><Abstract><AbstractText>"
                "DESeq2 estimates size factors for RNA-seq count data."
                "</AbstractText></Abstract></Article></MedlineCitation>"
                "</PubmedArticle></PubmedArticleSet>"
            )
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="normalisation",
                query="DESeq2 normalization RNA-seq",
                classes=["primary_literature"],
                routes={"primary_literature": {"hosts": ["eutils.ncbi.nlm.nih.gov"]}},
                curated=["deseq2_vst"],
                candidate="deseq2_vst",
            )
            alf._http_get_json = lambda url, host, allowed: {"results": []}
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="pathway_enrichment",
                query="fgsea preranked enrichment",
                classes=["conference_proceedings"],
                routes={"conference_proceedings": {"hosts": ["api.openalex.org"]}},
                curated=["fgsea"],
                candidate="fgsea",
            )

            rows = _read_csv_rows(out)
            self.assertEqual(
                {row["source_class"] for row in rows},
                {"primary_literature", "curated_baseline"},
            )
            for row in rows:
                typed = dict(row)
                typed["evidence_quote_offset"] = int(typed["evidence_quote_offset"])
                typed["redistributable"] = typed["redistributable"].lower() == "true"
                typed["verified"] = typed["verified"].lower() == "true"
                jsonschema.validate(typed, schema)


class DefaultClassesTest(unittest.TestCase):
    def test_default_class_is_primary_literature_when_scope_unset(self):
        # When `classes` is None, the helper derives the enabled set from
        # the environment, defaulting to primary_literature only.
        os.environ.pop("ECAA_LIT_SOURCE_SCOPE", None)
        self.assertEqual(alf.enabled_classes(None), ["primary_literature"])


def _read_json(out: Path):
    return json.loads((out / "method_landscape.json").read_text())


class CuratedFallbackTest(unittest.TestCase):
    def test_zero_usable_rows_emits_curated_baseline_rows(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()

            # Fetch returns an empty result set: no usable rows for the axis.
            def empty_get_json(url, host, allowed_hosts):
                assert host in allowed_hosts
                return {"results": []}

            alf._http_get_json = empty_get_json
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="alignment",
                query="STAR RNA-seq aligner",
                classes=["conference_proceedings"],
                routes={"conference_proceedings": {"hosts": ["api.openalex.org"]}},
                curated=["star", "hisat2"],
            )

            rows = _read_csv_rows(out)
            # One curated_baseline row per curated candidate.
            self.assertEqual(len(rows), 2)
            cands = sorted(r["candidate_method"] for r in rows)
            self.assertEqual(cands, ["hisat2", "star"])
            for r in rows:
                self.assertEqual(r["source_class"], "curated_baseline")
                self.assertEqual(r["verified"], "false")
                # No locator: source_ref_kind / source_ref empty.
                self.assertEqual(r["source_ref_kind"], "")
                self.assertEqual(r["source_ref"], "")
                self.assertEqual(r["evidence_quote"], "")
                self.assertEqual(r["axis"], "alignment")

    def test_fetch_raising_falls_back_to_curated_and_does_not_raise(self):
        import tempfile
        import urllib.error

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()

            def boom_get_json(url, host, allowed_hosts):
                # Transport failure (offline / route down), NOT an allowlist
                # violation — must degrade to the curated fallback.
                raise urllib.error.URLError("simulated offline / route failure")

            alf._http_get_json = boom_get_json
            # Must NOT raise even though every route fails.
            summary = alf.fetch_for_axis(
                out_dir=str(out),
                axis="alignment",
                query="STAR",
                classes=["conference_proceedings"],
                routes={"conference_proceedings": {"hosts": ["api.openalex.org"]}},
                curated=["star", "salmon"],
            )
            self.assertTrue(summary.get("fallback_used"))
            rows = _read_csv_rows(out)
            self.assertEqual(len(rows), 2)
            for r in rows:
                self.assertEqual(r["source_class"], "curated_baseline")

    def test_allowlist_violation_propagates_not_silently_falls_back(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()

            def reject_host(url, host, allowed_hosts):
                raise alf.HostNotAllowedError("misconfigured route allowlist")

            alf._http_get_json = reject_host
            # An egress-allowlist violation is a misconfiguration, not an
            # availability failure: it must surface loudly, not degrade.
            with self.assertRaises(alf.HostNotAllowedError):
                alf.fetch_for_axis(
                    out_dir=str(out),
                    axis="alignment",
                    query="STAR",
                    classes=["conference_proceedings"],
                    routes={"conference_proceedings": {"hosts": ["api.openalex.org"]}},
                    curated=["star"],
                )

    def test_fallback_method_landscape_json_marks_tentative_and_zero_score(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            alf._http_get_json = lambda url, host, allowed: {"results": []}
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="alignment",
                query="STAR",
                classes=["conference_proceedings"],
                routes={"conference_proceedings": {"hosts": ["api.openalex.org"]}},
                curated=["star", "hisat2"],
            )
            doc = _read_json(out)
            self.assertEqual(doc["schema_version"], 1)
            cands = doc["axes"]["alignment"]["candidates"]
            self.assertEqual(len(cands), 2)
            by_name = {c["method"]: c for c in cands}
            star = by_name["star"]
            # curated_baseline → not literature_eligible, support_score 0.
            self.assertFalse(star["literature_eligible"])
            self.assertEqual(star["support_score"], 0)
            # In the curated pool → NOT tentative.
            self.assertFalse(star["tentative"])
            # The single evidence row is the curated_baseline marker.
            self.assertEqual(len(star["evidence"]), 1)
            ev = star["evidence"][0]
            self.assertEqual(ev["source_class"], "curated_baseline")
            self.assertIsNone(ev["source_ref_kind"])
            self.assertIsNone(ev["source_ref"])


class MethodLandscapeJsonRollupTest(unittest.TestCase):
    def test_normal_path_emits_method_landscape_json_shape(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()

            alf._http_get_json = lambda url, host, allowed: {
                "results": [
                    {
                        "doi": "https://doi.org/10.1/x",
                        "display_name": "STAR aligner",
                        "abstract_inverted_index": {"STAR": [0], "aligns": [1]},
                    }
                ]
            }
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="alignment",
                query="STAR",
                classes=["conference_proceedings"],
                routes={"conference_proceedings": {"hosts": ["api.openalex.org"]}},
                curated=["star"],
            )
            doc = _read_json(out)
            self.assertEqual(doc["schema_version"], 1)
            self.assertIn("alignment", doc["axes"])
            cands = doc["axes"]["alignment"]["candidates"]
            self.assertEqual(len(cands), 1)
            c = cands[0]
            self.assertEqual(c["method"], "STAR aligner")
            # conference_proceedings verified row → literature_eligible,
            # support_score 1 (one paper-class verified evidence row).
            self.assertTrue(c["literature_eligible"])
            self.assertEqual(c["support_score"], 1)
            # "STAR aligner" is not literally in the curated pool ["star"] →
            # tentative.
            self.assertTrue(c["tentative"])
            self.assertEqual(len(c["evidence"]), 1)
            ev = c["evidence"][0]
            self.assertEqual(ev["source_class"], "conference_proceedings")
            self.assertEqual(ev["source_ref_kind"], "doi")
            self.assertEqual(ev["source_ref"], "10.1/x")
            self.assertTrue(isinstance(ev["evidence_quote"], str))

    def test_multi_axis_calls_preserve_each_axis_curated_pool(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            # Axis 1: a verified OpenAlex hit whose name IS in the curated pool.
            alf._http_get_json = lambda url, host, allowed: {
                "results": [
                    {
                        "doi": "https://doi.org/10.1/star",
                        "display_name": "STAR",
                        "abstract_inverted_index": {"STAR": [0], "aligns": [1]},
                    }
                ]
            }
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="alignment",
                query="STAR",
                classes=["conference_proceedings"],
                routes={"conference_proceedings": {"hosts": ["api.openalex.org"]}},
                curated=["STAR", "hisat2"],
            )
            # Axis 2: offline → curated fallback.
            alf._http_get_json = lambda url, host, allowed: {"results": []}
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="quantification",
                query="salmon",
                classes=["conference_proceedings"],
                routes={"conference_proceedings": {"hosts": ["api.openalex.org"]}},
                curated=["salmon"],
            )
            doc = _read_json(out)
            # The second call rebuilds from the full CSV; axis-1's STAR must
            # still be non-tentative because its curated pool was persisted.
            star = doc["axes"]["alignment"]["candidates"][0]
            self.assertEqual(star["method"], "STAR")
            self.assertFalse(star["tentative"], "axis-1 curated pool must survive axis-2 rebuild")

    def test_per_candidate_fallback_does_not_repeat_full_axis_pool(self):
        import csv
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            alf._http_get_json = lambda url, host, allowed: {"results": []}
            route = {"conference_proceedings": {"hosts": ["api.openalex.org"]}}
            curated = ["fgsea", "clusterprofiler", "gsea", "enrichr"]

            alf.fetch_for_axis(
                out_dir=str(out),
                axis="pathway_enrichment",
                query="fgsea preranked",
                classes=["conference_proceedings"],
                routes=route,
                curated=curated,
                candidate="fgsea",
            )
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="pathway_enrichment",
                query="gsea preranked",
                classes=["conference_proceedings"],
                routes=route,
                curated=curated,
                candidate="gsea",
            )

            with (out / "method_landscape.csv").open(newline="") as handle:
                rows = list(csv.DictReader(handle))
            self.assertEqual(
                [row["candidate_method"] for row in rows],
                ["fgsea", "gsea"],
            )
            self.assertTrue(all(row["source_class"] == "curated_baseline" for row in rows))
            pools = json.loads((out / "evidence" / "curated_pools.json").read_text())
            self.assertEqual(pools["pathway_enrichment"], curated)

    def test_candidates_sorted_by_support_score_then_name(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            # Two OpenAlex hits for two distinct candidates; both verified
            # (support_score 1 each) → tie broken by name ascending.
            alf._http_get_json = lambda url, host, allowed: {
                "results": [
                    {
                        "doi": "https://doi.org/10.1/b",
                        "display_name": "zztool",
                        "abstract_inverted_index": {"zztool": [0], "aligns": [1]},
                    },
                    {
                        "doi": "https://doi.org/10.1/a",
                        "display_name": "aatool",
                        "abstract_inverted_index": {"aatool": [0], "aligns": [1]},
                    },
                ]
            }
            alf.fetch_for_axis(
                out_dir=str(out),
                axis="alignment",
                query="aligner",
                classes=["conference_proceedings"],
                routes={"conference_proceedings": {"hosts": ["api.openalex.org"]}},
                curated=[],
            )
            doc = _read_json(out)
            names = [c["method"] for c in doc["axes"]["alignment"]["candidates"]]
            # Equal support_score (1) → name ascending.
            self.assertEqual(names, ["aatool", "zztool"])


class CliCuratedFlagTest(unittest.TestCase):
    def test_main_parses_curated_flag_and_falls_back(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            alf._http_get_json = lambda url, host, allowed: {"results": []}
            rc = alf._main(
                [
                    "agent_literature_fetch.py",
                    str(out),
                    "alignment",
                    "STAR",
                    "conference_proceedings",
                    "--curated",
                    "star,hisat2",
                ]
            )
            self.assertEqual(rc, 0)
            rows = _read_csv_rows(out)
            self.assertEqual(sorted(r["candidate_method"] for r in rows), ["hisat2", "star"])
            for r in rows:
                self.assertEqual(r["source_class"], "curated_baseline")


if __name__ == "__main__":
    unittest.main()
