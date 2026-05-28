from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import yaml


REPO = Path(__file__).resolve().parents[1]
HARNESS = (
    Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target"))
    / "debug"
    / "ecaa-workflow-harness"
)
ALT_HARNESS = None  # operator-local mount fallback removed for OSS distribution
AGENT = REPO / "scripts" / "agent-fixture-plots.sh"
PASILLA = REPO / "testdata" / "scenarios" / "atoms" / "bulk-rnaseq-pasilla"
CONTAINER_IMAGE = "bio-min:local"


FIGURES_BY_TASK = {
    "data_acquisition": ["samples_per_study"],
    "raw_qc": [
        "per_sample_metric_violin",
        "per_sample_metric_bar",
        "qc_summary_bar",
    ],
    "qc_preprocessing": [
        "per_sample_metric_violin",
        "per_sample_metric_bar",
        "qc_summary_bar",
    ],
    "normalisation": ["mean_variance", "hvg_count_bar", "sample_pca"],
    "differential_expression": ["volcano", "top_features_heatmap"],
    "pathway_enrichment": ["top_enriched_terms"],
    "reporting": ["concordance_heatmap", "pathway_overlap_bar"],
    "final_reporting": ["summary_dashboard"],
}


def write_json(path: Path, body: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(body, indent=2, sort_keys=True) + "\n")


def run(cmd: list[str], cwd: Path, env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )


def harness_bin() -> Path:
    if HARNESS.exists():
        return HARNESS
    raise AssertionError(f"harness binary not found at {HARNESS}")


def task_spec(
    task_id: str,
    kind: object,
    required: list[str] | None = None,
    plot_stage_id: str | None = None,
) -> dict[str, object]:
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


def workflow_task(task_id: str, kind: object, depends_on: list[str]) -> dict[str, object]:
    return {
        "kind": kind,
        "state": {"status": "pending"},
        "depends_on": depends_on,
        "assignee": "agent",
        "description": f"Fixture execution for {task_id}",
        "resource_class": "cpu_heavy",
    }


def build_pasilla_package(pkg: Path) -> list[str]:
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
    write_json(pkg / "policies" / "container.json", {"image": CONTAINER_IMAGE})

    edges = {
        "data_acquisition": [],
        "raw_qc": ["data_acquisition"],
        "validate_raw_qc": ["raw_qc"],
        "qc_preprocessing": ["data_acquisition"],
        "normalisation": ["qc_preprocessing"],
        "differential_expression": ["normalisation"],
        "pathway_enrichment": ["differential_expression"],
        "reporting": ["pathway_enrichment", "raw_qc"],
        "final_reporting": ["reporting"],
        "validate_final_reporting": ["final_reporting"],
    }
    order = list(edges)
    kinds = {
        task_id: "validation" if task_id.startswith("validate_") else "computation"
        for task_id in order
    }
    specs = {
        "data_acquisition": task_spec("data_acquisition", "computation", ["samples_per_study"]),
        "raw_qc": task_spec(
            "raw_qc",
            "computation",
            FIGURES_BY_TASK["raw_qc"],
            "quality_control",
        ),
        "qc_preprocessing": task_spec(
            "qc_preprocessing",
            "computation",
            FIGURES_BY_TASK["qc_preprocessing"],
            "quality_control",
        ),
        "normalisation": task_spec(
            "normalisation",
            "computation",
            FIGURES_BY_TASK["normalisation"],
            "normalization",
        ),
        "differential_expression": task_spec(
            "differential_expression",
            "computation",
            FIGURES_BY_TASK["differential_expression"],
        ),
        "pathway_enrichment": task_spec(
            "pathway_enrichment",
            "computation",
            FIGURES_BY_TASK["pathway_enrichment"],
            "biological_interpretation",
        ),
        "reporting": task_spec("reporting", "computation", FIGURES_BY_TASK["reporting"]),
        "final_reporting": task_spec(
            "final_reporting", "computation", FIGURES_BY_TASK["final_reporting"]
        ),
    }
    for task_id in order:
        specs.setdefault(task_id, task_spec(task_id, kinds[task_id]))

    write_json(
        pkg / "WORKFLOW.json",
        {
            "version": "1",
            "workflow_id": "fixture-pasilla-harness",
            "current_task": None,
            "tasks": {
                task_id: workflow_task(task_id, kinds[task_id], deps)
                for task_id, deps in edges.items()
            },
        },
    )
    for task_id, spec in specs.items():
        write_json(outputs / task_id / "task-spec.json", spec)
    return order


def assert_no_isolated_nodes(workflow: dict[str, object]) -> None:
    tasks = workflow["tasks"]
    assert isinstance(tasks, dict)
    outgoing = {task_id: 0 for task_id in tasks}
    for task_id, task in tasks.items():
        for dep in task.get("depends_on", []):
            assert dep in tasks, f"{task_id} depends on missing task {dep}"
            outgoing[dep] += 1
    isolated = [
        task_id
        for task_id, task in tasks.items()
        if not task.get("depends_on") and outgoing[task_id] == 0 and len(tasks) > 1
    ]
    assert isolated == []


def assert_figure(pkg: Path, task_id: str, figure_id: str) -> None:
    figures = pkg / "runtime" / "outputs" / task_id / "figures"
    png = figures / f"{figure_id}.png"
    pdf = figures / f"{figure_id}.pdf"
    assert png.read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
    assert png.stat().st_size > 4096
    assert pdf.read_bytes().startswith(b"%PDF-")
    assert pdf.stat().st_size > 512
    manifest = json.loads((figures / "manifest.json").read_text())
    assert manifest["written"][figure_id].endswith(f"{figure_id}.png")
    assert figure_id in manifest["formats"]
    assert not manifest["errors"], manifest["errors"]


def completed_task_events(pkg: Path) -> list[str]:
    events: list[str] = []
    for line in (pkg / "runtime" / "LOG.jsonl").read_text().splitlines():
        if not line.strip():
            continue
        event = json.loads(line)
        if event.get("event") == "completed" and event.get("agent") == "fixture-plots":
            events.append(str(event["task"]))
    return events


def assert_completion_order_respects_dependencies(
    workflow: dict[str, object], completed_events: list[str]
) -> None:
    tasks = workflow["tasks"]
    assert isinstance(tasks, dict)
    position = {task_id: idx for idx, task_id in enumerate(completed_events)}
    assert set(position) == set(tasks)
    for task_id, task in tasks.items():
        for dep in task.get("depends_on", []):
            assert position[dep] < position[task_id], (
                f"{task_id} completed before dependency {dep}: {completed_events}"
            )


def assert_container_and_usage(pkg: Path, task_id: str) -> None:
    out = pkg / "runtime" / "outputs" / task_id
    container = json.loads((out / ".container-state.json").read_text())
    assert container["runtime"] == "docker"
    assert container["image"] == CONTAINER_IMAGE
    assert container["exit_code"] == 0
    proof = json.loads((out / "container-proof.json").read_text())
    assert proof["containerized"] is True
    assert proof["runtime"] == "docker"
    usage = json.loads((out / "agent-usage.json").read_text())
    assert usage["num_turns"] == 0
    assert usage["input_tokens"] == 0
    assert usage["output_tokens"] == 0
    assert usage["total_cost_usd"] == 0


def commit_package_artifacts(pkg: Path) -> None:
    run(["git", "init"], cwd=pkg)
    run(["git", "config", "user.name", "Scripps Fixture Agent"], cwd=pkg)
    run(["git", "config", "user.email", "fixture-agent@scripps.local"], cwd=pkg)
    run(["git", "add", "-A"], cwd=pkg)
    run(["git", "commit", "-m", "test: commit fixture harness artifacts"], cwd=pkg)
    status = run(["git", "status", "--porcelain"], cwd=pkg).stdout.strip()
    assert status == ""
    tree = run(["git", "ls-tree", "-r", "--name-only", "HEAD"], cwd=pkg).stdout.splitlines()
    assert "runtime/outputs/differential_expression/figures/volcano.png" in tree
    assert "runtime/outputs/final_reporting/figures/summary_dashboard.pdf" in tree


def load_atom_catalog() -> list[dict[str, object]]:
    atoms: list[dict[str, object]] = []
    atom_dir = REPO / "config" / "stage-atoms"
    for path in sorted(atom_dir.glob("*.yaml")):
        if path.name.startswith("_") or path.name == "README.md":
            continue
        data = yaml.safe_load(path.read_text())
        if not isinstance(data, dict):
            continue
        atom_id = data.get("id")
        if not isinstance(atom_id, str) or not atom_id:
            raise AssertionError(f"atom missing string id: {path}")
        atoms.append(data)
    assert atoms, "atom catalog must not be empty"
    return atoms


def build_all_atom_catalog_package(pkg: Path) -> list[dict[str, object]]:
    runtime = pkg / "runtime"
    outputs = runtime / "outputs"
    shutil.copytree(REPO / "lib" / "plotting", runtime / "plotting")
    write_json(pkg / "policies" / "container.json", {"image": CONTAINER_IMAGE})

    atoms = load_atom_catalog()
    atom_ids = [str(atom["id"]) for atom in atoms]
    root_task_id = atom_ids[0]
    sink_task_id = "final_reporting" if "final_reporting" in atom_ids else atom_ids[-1]
    non_sink_ids = [atom_id for atom_id in atom_ids if atom_id != sink_task_id]
    tasks: dict[str, dict[str, object]] = {}
    for atom in atoms:
        atom_id = str(atom["id"])
        if atom_id == root_task_id:
            depends_on: list[str] = []
        elif atom_id == sink_task_id:
            depends_on = [dep for dep in non_sink_ids if dep != atom_id]
        else:
            depends_on = [root_task_id]
        tasks[atom_id] = workflow_task(atom_id, "computation", depends_on)
        tasks[atom_id]["atom_id"] = atom_id
        spec: dict[str, object] = {}
        required = atom.get("required_figures") or []
        if required:
            spec["required_figures"] = required
        plot_stage_id = atom.get("plot_stage_id")
        if plot_stage_id:
            spec["plot_stage_id"] = plot_stage_id
        write_json(
            outputs / atom_id / "task-spec.json",
            {
                "task_id": atom_id,
                "kind": "computation",
                "spec": spec,
                "state": {"status": "pending"},
            },
        )

    write_json(
        pkg / "WORKFLOW.json",
        {
            "version": "1",
            "workflow_id": "fixture-all-atom-catalog",
            "current_task": None,
            "tasks": tasks,
        },
    )
    assert_no_isolated_nodes({"tasks": tasks})
    return atoms


def test_local_harness_executes_every_atom_catalog_task_and_plot(tmp_path: Path) -> None:
    pkg = tmp_path / "all-atom-catalog-package"
    atoms = build_all_atom_catalog_package(pkg)

    env = {
        **os.environ,
        "ECAA_EXECUTOR_MODE": "local",
        "ECAA_DEFAULT_CONTAINER_IMAGE": CONTAINER_IMAGE,
        "ECAA_HARNESS_CONCURRENCY": "6",
        "ECAA_HARNESS_VALIDATION_LANE": "0",
        "ECAA_HARNESS_SETTLE_SECS": "0",
        "ECAA_LOCAL_SANDBOX": "off",
        "ECAA_DISABLE_CONTAINERS": "0",
        "ECAA_REQUIRE_CONTAINER_EXECUTION": "1",
        "ECAA_DISABLE_ENV_CLEAR": "0",
        "OMP_NUM_THREADS": "1",
        "OPENBLAS_NUM_THREADS": "1",
        "MKL_NUM_THREADS": "1",
        "NUMEXPR_NUM_THREADS": "1",
    }
    result = run(
        [
            str(harness_bin()),
            "--package",
            str(pkg),
            "--agent",
            str(AGENT),
            "--max-iterations",
            str(len(atoms) + 8),
            "--task-timeout",
            "180",
            "--no-interactive",
        ],
        cwd=REPO,
        env=env,
    )
    assert "All tasks complete" in result.stdout

    workflow = json.loads((pkg / "WORKFLOW.json").read_text())
    assert_no_isolated_nodes(workflow)
    assert {t["state"]["status"] for t in workflow["tasks"].values()} == {"completed"}

    completed_events = completed_task_events(pkg)
    assert set(completed_events) == {str(atom["id"]) for atom in atoms}
    assert_completion_order_respects_dependencies(workflow, completed_events)

    for atom in atoms:
        task_id = str(atom["id"])
        assert_container_and_usage(pkg, task_id)
        for figure_id in atom.get("required_figures") or []:
            assert_figure(pkg, task_id, str(figure_id))

    commit_package_artifacts(pkg)


def test_local_harness_executes_containerized_pasilla_plot_dag(tmp_path: Path) -> None:
    pkg = tmp_path / "pasilla-package"
    expected_order = build_pasilla_package(pkg)

    env = {
        **os.environ,
        "ECAA_EXECUTOR_MODE": "local",
        "ECAA_DEFAULT_CONTAINER_IMAGE": CONTAINER_IMAGE,
        "ECAA_HARNESS_CONCURRENCY": "1",
        "ECAA_HARNESS_VALIDATION_LANE": "0",
        "ECAA_HARNESS_SETTLE_SECS": "0",
        "ECAA_LOCAL_SANDBOX": "off",
        "ECAA_DISABLE_CONTAINERS": "0",
        "ECAA_DISABLE_ENV_CLEAR": "0",
        "OMP_NUM_THREADS": "1",
        "OPENBLAS_NUM_THREADS": "1",
        "MKL_NUM_THREADS": "1",
        "NUMEXPR_NUM_THREADS": "1",
    }
    result = run(
        [
            str(harness_bin()),
            "--package",
            str(pkg),
            "--agent",
            str(AGENT),
            "--max-iterations",
            "32",
            "--task-timeout",
            "180",
            "--no-interactive",
        ],
        cwd=REPO,
        env=env,
    )
    assert "All tasks complete" in result.stdout

    workflow = json.loads((pkg / "WORKFLOW.json").read_text())
    assert_no_isolated_nodes(workflow)
    assert {t["state"]["status"] for t in workflow["tasks"].values()} == {"completed"}
    completed_events = completed_task_events(pkg)
    assert_completion_order_respects_dependencies(workflow, completed_events)
    assert completed_events[0] == expected_order[0]

    for task_id, figure_ids in FIGURES_BY_TASK.items():
        assert_container_and_usage(pkg, task_id)
        for figure_id in figure_ids:
            assert_figure(pkg, task_id, figure_id)
    for task_id in expected_order:
        assert_container_and_usage(pkg, task_id)

    commit_package_artifacts(pkg)
