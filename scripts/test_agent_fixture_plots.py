from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path


REPO = Path(__file__).resolve().parents[1]
AGENT = REPO / "scripts" / "agent-fixture-plots.sh"
PASILLA = REPO / "testdata" / "scenarios" / "atoms" / "bulk-rnaseq-pasilla"


def write_json(path: Path, body: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(body, indent=2, sort_keys=True) + "\n")


def task_spec(
    task_id: str,
    kind: object,
    required: list[str] | None = None,
    plot_stage_id: str | None = None,
) -> dict:
    spec: dict[str, object] = {}
    if required:
        spec["required_figures"] = required
    if plot_stage_id:
        spec["plot_stage_id"] = plot_stage_id
    return {
        "task_id": task_id,
        "kind": kind,
        "spec": spec,
        "state": {"status": "pending"},
    }


def apply_patch_to_workflow(pkg: Path, task_id: str) -> None:
    workflow_path = pkg / "WORKFLOW.json"
    workflow = json.loads(workflow_path.read_text())
    patch_path = pkg / "runtime" / "outputs" / task_id / "state.patch.json"
    patch = json.loads(patch_path.read_text())
    workflow["tasks"][task_id]["state"] = patch["to"]
    workflow_path.write_text(json.dumps(workflow, indent=2, sort_keys=True) + "\n")


def run_task(pkg: Path, task_id: str) -> None:
    workflow_path = pkg / "WORKFLOW.json"
    workflow = json.loads(workflow_path.read_text())
    workflow["tasks"][task_id]["state"] = {"status": "running"}
    workflow_path.write_text(json.dumps(workflow, indent=2, sort_keys=True) + "\n")
    env = {
        **os.environ,
        "SWFC_TASK_ID": task_id,
        "SWFC_HARNESS_RUN_ID": "fixture-run",
        "SWFC_DISPATCH_EPOCH": "1",
    }
    subprocess.run([str(AGENT), str(pkg)], check=True, cwd=REPO, env=env)
    apply_patch_to_workflow(pkg, task_id)


def assert_figure(pkg: Path, task_id: str, figure_id: str) -> None:
    figures = pkg / "runtime" / "outputs" / task_id / "figures"
    assert (figures / f"{figure_id}.png").stat().st_size > 0
    assert (figures / f"{figure_id}.pdf").stat().st_size > 0
    manifest = json.loads((figures / "manifest.json").read_text())
    assert figure_id in manifest["written"]
    assert not manifest["errors"], manifest["errors"]


def test_fixture_agent_executes_pasilla_plot_tasks(tmp_path: Path) -> None:
    pkg = tmp_path / "pkg"
    runtime = pkg / "runtime"
    outputs = runtime / "outputs"
    shutil.copytree(REPO / "lib" / "plotting", runtime / "plotting")

    staged = runtime / "inputs" / "pasilla"
    staged.mkdir(parents=True)
    shutil.copy2(PASILLA / "counts.tsv", staged / "pasilla_gene_counts.tsv")
    shutil.copy2(PASILLA / "samples.csv", staged / "pasilla_sample_annotation.csv")
    shutil.copy2(PASILLA / "drosophila_pasilla_mini.gmt", staged / "drosophila_pasilla_mini.gmt")
    write_json(
        runtime / "inputs.json",
        [
            {
                "input_id": "pasilla",
                "label": "pasilla",
                "kind": "local_path",
                "root_path": str(staged),
                "files": [
                    {"relpath": "pasilla_gene_counts.tsv"},
                    {"relpath": "pasilla_sample_annotation.csv"},
                    {"relpath": "drosophila_pasilla_mini.gmt"},
                ],
            }
        ],
    )

    specs = {
        "data_acquisition": task_spec("data_acquisition", "computation", ["samples_per_study"]),
        "discover_differential_expression": task_spec(
            "discover_differential_expression", {"discovery": "best_practice"}
        ),
        "discover_normalisation": task_spec(
            "discover_normalisation", {"discovery": "best_practice"}
        ),
        "discover_pathway_enrichment": task_spec(
            "discover_pathway_enrichment", {"discovery": "best_practice"}
        ),
        "qc_preprocessing": task_spec(
            "qc_preprocessing",
            "computation",
            ["per_sample_metric_violin", "per_sample_metric_bar", "qc_summary_bar"],
            "quality_control",
        ),
        "normalisation": task_spec(
            "normalisation",
            "computation",
            ["mean_variance", "hvg_count_bar", "sample_pca"],
            "normalization",
        ),
        "differential_expression": task_spec(
            "differential_expression", "computation", ["volcano", "top_features_heatmap"]
        ),
        "pathway_enrichment": task_spec(
            "pathway_enrichment", "computation", ["top_enriched_terms"], "biological_interpretation"
        ),
        "reporting": task_spec(
            "reporting", "computation", ["concordance_heatmap", "pathway_overlap_bar"]
        ),
        "final_reporting": task_spec("final_reporting", "computation", ["summary_dashboard"]),
        "validate_data_acquisition": task_spec("validate_data_acquisition", "validation"),
        "validate_qc_preprocessing": task_spec("validate_qc_preprocessing", "validation"),
        "validate_normalisation": task_spec("validate_normalisation", "validation"),
        "validate_differential_expression": task_spec(
            "validate_differential_expression", "validation"
        ),
        "validate_pathway_enrichment": task_spec("validate_pathway_enrichment", "validation"),
        "validate_reporting": task_spec("validate_reporting", "validation"),
        "validate_final_reporting": task_spec("validate_final_reporting", "validation"),
    }
    workflow = {
        "version": "1",
        "workflow_id": "fixture-pasilla",
        "tasks": {
            task_id: {"state": {"status": "pending"}, "kind": spec["kind"], "depends_on": []}
            for task_id, spec in specs.items()
        },
    }
    write_json(pkg / "WORKFLOW.json", workflow)
    for task_id, spec in specs.items():
        write_json(outputs / task_id / "task-spec.json", spec)

    order = [
        "data_acquisition",
        "discover_differential_expression",
        "discover_normalisation",
        "discover_pathway_enrichment",
        "qc_preprocessing",
        "normalisation",
        "differential_expression",
        "pathway_enrichment",
        "reporting",
        "final_reporting",
        "validate_data_acquisition",
        "validate_qc_preprocessing",
        "validate_normalisation",
        "validate_differential_expression",
        "validate_pathway_enrichment",
        "validate_reporting",
        "validate_final_reporting",
    ]
    for task_id in order:
        run_task(pkg, task_id)

    final_workflow = json.loads((pkg / "WORKFLOW.json").read_text())
    assert {t["state"]["status"] for t in final_workflow["tasks"].values()} == {"completed"}

    assert_figure(pkg, "data_acquisition", "samples_per_study")
    assert_figure(pkg, "qc_preprocessing", "per_sample_metric_violin")
    assert_figure(pkg, "qc_preprocessing", "per_sample_metric_bar")
    assert_figure(pkg, "qc_preprocessing", "qc_summary_bar")
    assert_figure(pkg, "normalisation", "mean_variance")
    assert_figure(pkg, "normalisation", "hvg_count_bar")
    assert_figure(pkg, "normalisation", "sample_pca")
    assert_figure(pkg, "differential_expression", "volcano")
    assert_figure(pkg, "differential_expression", "top_features_heatmap")
    assert_figure(pkg, "pathway_enrichment", "top_enriched_terms")
    assert_figure(pkg, "reporting", "concordance_heatmap")
    assert_figure(pkg, "reporting", "pathway_overlap_bar")
    assert_figure(pkg, "final_reporting", "summary_dashboard")
