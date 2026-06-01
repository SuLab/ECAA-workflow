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
            alf._http_get_text(
                "https://evil.example.com/x", "evil.example.com", ["readthedocs.io"]
            )


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
            import re

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
            self.assertFalse(
                star["tentative"], "axis-1 curated pool must survive axis-2 rebuild"
            )

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
            self.assertEqual(
                sorted(r["candidate_method"] for r in rows), ["hisat2", "star"]
            )
            for r in rows:
                self.assertEqual(r["source_class"], "curated_baseline")


if __name__ == "__main__":
    unittest.main()
