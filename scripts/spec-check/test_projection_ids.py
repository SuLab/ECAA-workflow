#!/usr/bin/env python3
"""Regression tests for URI-safe synthesized projection IDs."""

from rdflib import Graph
from pyld import jsonld

from _project import (
    load_context,
    project_blocker_row,
    project_evidence_outputs,
    project_claim_verdicts,
    project_nanopub,
    project_rerun_outcome_row,
)


def _serialize_node(node):
    doc = {"@context": load_context()["@context"], **node}
    nq = jsonld.to_rdf(doc, {"format": "application/n-quads"})
    graph = Graph()
    graph.parse(data=nq, format="nquads")
    return graph.serialize(format="turtle")


def test_rerun_and_blocker_ids_are_uri_safe_for_union_ids():
    raw = "unif:data:3134:union(data:0951|data:3002|data:3498)"
    rerun = project_rerun_outcome_row({"id": raw, "class": "failed"}, 1)
    blocker = project_blocker_row({"id": f"fail:{raw}", "refs": raw}, 2)

    assert rerun["id"] == (
        "ecaa:rerun:"
        "unif%3Adata%3A3134%3Aunion%28data%3A0951%7Cdata%3A3002%7Cdata%3A3498%29"
    )
    assert blocker["refs"]["@id"] == rerun["id"]

    _serialize_node(rerun)
    _serialize_node(blocker)


def test_claim_ids_are_uri_safe_for_fragment_like_ids():
    docs = project_claim_verdicts(
        {
            "verdicts": [
                {
                    "claim_id": "differential_expression#claim-0",
                    "status": "verified",
                    "supported_by": ["runtime/outputs/differential_expression/de_table.tsv"],
                }
            ]
        }
    )
    assert docs[0]["id"] == "ecaa:claim:differential_expression%23claim-0"
    _serialize_node(docs[0])

    nanopubs = project_nanopub(
        {
            "verdicts": [
                {
                    "claim_id": "differential_expression#claim-0",
                    "status": "verified",
                    "supported_by": ["runtime/outputs/differential_expression/de_table.tsv"],
                }
            ]
        }
    )
    assert any(
        node.get("id") == "ecaa:nanopub:differential_expression%23claim-0"
        for node in nanopubs
    )


def test_evidence_outputs_include_rocrate_runtime_outputs():
    outputs = project_evidence_outputs(
        [{"computed_from": "workflow:data_acquisition"}],
        [
            {
                "@id": "runtime/outputs/differential_expression/differential_expression.tsv",
                "@type": ["File", "Dataset"],
            },
            {
                "@id": "runtime/outputs/differential_expression/figures/volcano.png",
                "@type": ["File", "ImageObject"],
            },
            {"@id": "policies/runtime-prereqs.json", "@type": ["File"]},
        ],
    )

    ids = {node["id"] for node in outputs}
    assert ids == {
        "runtime/outputs/differential_expression/differential_expression.tsv",
        "runtime/outputs/differential_expression/figures/volcano.png",
    }


def test_evidence_outputs_skip_planned_rocrate_outputs_until_files_exist(tmp_path):
    graph_nodes = [
        {
            "@id": "runtime/outputs/differential_expression/figures/volcano.png",
            "@type": ["File", "ImageObject"],
        }
    ]

    assert project_evidence_outputs([], graph_nodes, tmp_path) == []

    produced = tmp_path / "runtime/outputs/differential_expression/figures/volcano.png"
    produced.parent.mkdir(parents=True)
    produced.write_bytes(b"png")

    outputs = project_evidence_outputs([], graph_nodes, tmp_path)
    assert outputs == [
        {
            "id": "runtime/outputs/differential_expression/figures/volcano.png",
            "type": "OutputFile",
        }
    ]


def test_output_unused_blocker_projects_to_output_ref():
    output = "runtime/outputs/differential_expression/figures/volcano.png"
    blocker = project_blocker_row(
        {
            "id": "fixture_output_unused:differential_expression:abc123",
            "kind": "output_unused",
            "detail": output,
        },
        1,
    )

    assert blocker["kind"] == "OutputUnused"
    assert blocker["refs"] == {"@id": output}
    _serialize_node(blocker)
