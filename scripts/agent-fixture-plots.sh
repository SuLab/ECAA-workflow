#!/usr/bin/env bash
# agent-fixture-plots.sh — deterministic local execution agent for fixture
# workflows that exercise harness ordering and plotting without LLM tokens.
set -euo pipefail

PACKAGE="$(realpath "$1")"
SCRIPT_PATH="$(realpath "$0")"

resolve_fixture_container_image() {
  local image=""
  local workflow_json="$PACKAGE/WORKFLOW.json"
  local task_id="${SWFC_TASK_ID:-}"

  if [ -n "$task_id" ] && [ -f "$workflow_json" ] && command -v jq >/dev/null 2>&1; then
    image="$(jq -r --arg tid "$task_id" '
      (.tasks[$tid].container // empty) as $c
      | if ($c | type) == "object" then
          ($c.image // "") as $img
          | ($c.tag // "") as $tag
          | ($c.digest // "") as $digest
          | if $img == "" then ""
            elif $digest != "" then "\($img)@\($digest)"
            elif $tag != "" then "\($img):\($tag)"
            else $img
            end
        else ""
        end
    ' "$workflow_json" 2>/dev/null || true)"
  fi

  if [ -z "$image" ] && [ -f "$PACKAGE/policies/container.json" ] && command -v jq >/dev/null 2>&1; then
    image="$(jq -r '.image // empty' "$PACKAGE/policies/container.json" 2>/dev/null || true)"
  fi

  if [ -z "$image" ] && [ -n "${SWFC_DEFAULT_CONTAINER_IMAGE:-}" ]; then
    image="$SWFC_DEFAULT_CONTAINER_IMAGE"
  fi

  printf '%s\n' "$image"
}

if [ "${SWFC_FIXTURE_AGENT_CONTAINERIZED:-0}" != "1" ] \
   && [ "${SWFC_DISABLE_CONTAINERS:-0}" != "1" ]; then
  CONTAINER_IMAGE="$(resolve_fixture_container_image)"
  if [ -n "$CONTAINER_IMAGE" ]; then
    if ! command -v docker >/dev/null 2>&1; then
      echo "agent-fixture-plots.sh: docker is required for containerized fixture execution" >&2
      exit 97
    fi

    DOCKER_INPUT_ARGS=()
    if [ -f "$PACKAGE/runtime/inputs.json" ] && command -v jq >/dev/null 2>&1; then
      while IFS= read -r root_path; do
        [ -n "$root_path" ] || continue
        [ -e "$root_path" ] || continue
        case "$root_path" in
          "$PACKAGE"/*) continue ;;
        esac
        DOCKER_INPUT_ARGS+=(-v "$root_path":"$root_path":ro)
      done < <(jq -r '.[]? | select(.kind == "local_path") | .root_path // empty' "$PACKAGE/runtime/inputs.json" 2>/dev/null)
    fi

    DOCKER_ENV_ARGS=(
      -e SWFC_FIXTURE_AGENT_CONTAINERIZED=1
      -e "SWFC_FIXTURE_CONTAINER_IMAGE=$CONTAINER_IMAGE"
      -e "SWFC_TASK_ID=${SWFC_TASK_ID:-}"
      -e "SWFC_HARNESS_RUN_ID=${SWFC_HARNESS_RUN_ID:-}"
      -e "SWFC_DISPATCH_EPOCH=${SWFC_DISPATCH_EPOCH:-}"
      -e "SWFC_CHAT_SESSION_ID=${SWFC_CHAT_SESSION_ID:-}"
      -e "OMP_NUM_THREADS=${OMP_NUM_THREADS:-1}"
      -e "OPENBLAS_NUM_THREADS=${OPENBLAS_NUM_THREADS:-1}"
      -e "MKL_NUM_THREADS=${MKL_NUM_THREADS:-1}"
      -e "NUMEXPR_NUM_THREADS=${NUMEXPR_NUM_THREADS:-1}"
      -e MPLBACKEND=Agg
    )

    echo "agent-fixture-plots.sh: running fixture task ${SWFC_TASK_ID:-<auto>} inside $CONTAINER_IMAGE" >&2
    if ! docker image inspect "$CONTAINER_IMAGE" >/dev/null 2>&1; then
      docker pull "$CONTAINER_IMAGE" >/dev/null 2>&1 || true
    fi
    set +e
    docker run --rm \
      --read-only \
      --tmpfs /tmp:rw,size=1g,mode=1777 \
      --tmpfs /var/tmp:rw,size=1g,mode=1777 \
      --security-opt no-new-privileges \
      --cap-drop=ALL \
      --pids-limit 1024 \
      --user "$(id -u):$(id -g)" \
      -v "$PACKAGE":"$PACKAGE":rw \
      -v "$SCRIPT_PATH":/opt/scripps-agent-fixture-plots.sh:ro \
      "${DOCKER_INPUT_ARGS[@]}" \
      -w "$PACKAGE" \
      "${DOCKER_ENV_ARGS[@]}" \
      "$CONTAINER_IMAGE" \
      bash /opt/scripps-agent-fixture-plots.sh "$PACKAGE"
    rc=$?
    set -e

    if [ -n "${SWFC_TASK_ID:-}" ]; then
      state_dir="$PACKAGE/runtime/outputs/$SWFC_TASK_ID"
      mkdir -p "$state_dir" 2>/dev/null || true
      cat > "$state_dir/.container-state.json" 2>/dev/null <<EOF || true
{
  "exit_code": $rc,
  "image": "$CONTAINER_IMAGE",
  "runtime": "docker",
  "session_id": "${SWFC_CHAT_SESSION_ID:-}",
  "task_id": "${SWFC_TASK_ID}",
  "ended_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
    fi
    exit "$rc"
  fi

  if [ "${SWFC_REQUIRE_CONTAINER_EXECUTION:-0}" = "1" ]; then
    echo "agent-fixture-plots.sh: no container image resolved for ${SWFC_TASK_ID:-<auto>}" >&2
    exit 97
  fi
fi

python3 - "$PACKAGE" <<'PY'
from __future__ import annotations

import csv
import hashlib
import json
import math
import os
import shutil
import sys
from datetime import datetime, timezone
from pathlib import Path

PACKAGE = Path(sys.argv[1]).resolve()
WORKFLOW = PACKAGE / "WORKFLOW.json"
RUNTIME = PACKAGE / "runtime"
OUTPUTS = RUNTIME / "outputs"
TASK_ID = os.environ.get("SWFC_TASK_ID", "")


def now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


def load_json(path: Path, default=None):
    if not path.exists():
        return default
    return json.loads(path.read_text())


def write_json(path: Path, body) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(body, indent=2, sort_keys=True) + "\n")


def append(path: Path, line: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as f:
        f.write(line.rstrip("\n") + "\n")


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def task_status(workflow: dict, task_id: str) -> str:
    return (
        workflow.get("tasks", {})
        .get(task_id, {})
        .get("state", {})
        .get("status", "running")
    )


def choose_task(workflow: dict) -> str:
    if TASK_ID:
        return TASK_ID
    for tid, task in workflow.get("tasks", {}).items():
        if task.get("state", {}).get("status") in {"running", "ready"}:
            return tid
    return ""


def workflow_task_kind(workflow: dict, task_id: str) -> str:
    kind = workflow.get("tasks", {}).get(task_id, {}).get("kind")
    if isinstance(kind, str):
        return kind
    if isinstance(kind, dict):
        for key in ("type", "kind", "tag"):
            value = kind.get(key)
            if isinstance(value, str):
                return value
    return ""


def task_spec(task_id: str) -> dict:
    spec_path = OUTPUTS / task_id / "task-spec.json"
    return load_json(spec_path, {}) or {}


def required_figures(spec: dict) -> list[str]:
    stage_spec = spec.get("spec") if isinstance(spec.get("spec"), dict) else {}
    figs = stage_spec.get("required_figures") or spec.get("required_figures") or []
    return [str(f) for f in figs]


def plot_stage(task_id: str, spec: dict) -> str:
    stage_spec = spec.get("spec") if isinstance(spec.get("spec"), dict) else {}
    return str(stage_spec.get("plot_stage_id") or spec.get("plot_stage_id") or task_id)


def state_patch(task_id: str, from_status: str, to_state: dict) -> None:
    patch = {
        "schema_version": "0.1.0",
        "from": from_status,
        "to": to_state,
    }
    run_id = os.environ.get("SWFC_HARNESS_RUN_ID")
    epoch = os.environ.get("SWFC_DISPATCH_EPOCH")
    if run_id:
        patch["harness_run_id"] = run_id
    if epoch and epoch.isdigit():
        patch["dispatch_epoch"] = int(epoch)
    write_json(OUTPUTS / task_id / "state.patch.json", patch)


def complete(task_id: str, from_status: str, result: dict) -> None:
    out = OUTPUTS / task_id
    result = {"agent": "fixture-plots", "task_id": task_id, **result}
    write_json(out / "result.json", result)
    state_patch(task_id, from_status, {"status": "completed", "result": result})
    write_json(
        out / "agent-usage.json",
        {
            "model": "fixture-plots",
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_tokens": 0,
            "cache_creation_tokens": 0,
            "total_cost_usd": 0,
            "num_turns": 0,
        },
    )
    append(
        RUNTIME / "LOG.jsonl",
        json.dumps(
            {"ts": now(), "task": task_id, "event": "completed", "agent": "fixture-plots"},
            sort_keys=True,
        ),
    )


def block(task_id: str, from_status: str, reason: str) -> None:
    out = OUTPUTS / task_id
    write_json(
        out / "blocker.json",
        {
            "blocker_kind": "missing_input",
            "reason": reason,
            "task_id": task_id,
        },
    )
    state_patch(
        task_id,
        from_status,
        {"status": "blocked", "record": {"reason": reason, "attempts": []}},
    )
    append(
        RUNTIME / "LOG.jsonl",
        json.dumps(
            {"ts": now(), "task": task_id, "event": "blocked", "reason": reason},
            sort_keys=True,
        ),
    )


def write_container_proof(task_id: str) -> None:
    if os.environ.get("SWFC_FIXTURE_AGENT_CONTAINERIZED") != "1":
        return
    cgroup = ""
    try:
        cgroup = Path("/proc/1/cgroup").read_text()[:4096]
    except OSError:
        pass
    write_json(
        OUTPUTS / task_id / "container-proof.json",
        {
            "agent": "fixture-plots",
            "containerized": True,
            "image": os.environ.get("SWFC_FIXTURE_CONTAINER_IMAGE", ""),
            "runtime": "docker",
            "hostname": os.uname().nodename,
            "dockerenv": Path("/.dockerenv").exists(),
            "proc_1_cgroup": cgroup,
        },
    )


def load_inputs() -> list[dict]:
    return load_json(RUNTIME / "inputs.json", []) or []


def find_input_file(names: tuple[str, ...]) -> Path | None:
    for entry in load_inputs():
        root = Path(str(entry.get("root_path", "")))
        for file_entry in entry.get("files", []):
            rel = file_entry.get("relpath") or file_entry.get("relative_path")
            if not rel:
                continue
            path = root / str(rel)
            lowered = path.name.lower()
            if any(name in lowered for name in names) and path.exists():
                return path
    return None


def read_counts(path: Path) -> tuple[list[str], list[str], list[list[float]]]:
    with path.open(newline="") as f:
        reader = csv.reader(f, delimiter="\t")
        header = next(reader)
        samples = header[1:]
        genes: list[str] = []
        matrix: list[list[float]] = []
        for row in reader:
            if len(row) < len(header):
                continue
            genes.append(row[0])
            matrix.append([float(x) for x in row[1:]])
    return genes, samples, matrix


def read_gene_sets(path: Path) -> dict[str, set[str]]:
    sets: dict[str, set[str]] = {}
    if not path.exists():
        return sets
    with path.open() as f:
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) < 3:
                continue
            sets[parts[0]] = {gene for gene in parts[2:] if gene}
    return sets


def sample_condition(sample: str) -> str:
    low = sample.lower()
    if low.startswith("treated"):
        return "treated"
    if low.startswith("untreated"):
        return "untreated"
    return "unknown"


def ensure_data_acquisition() -> tuple[list[str], list[str], list[list[float]]]:
    counts = OUTPUTS / "data_acquisition" / "data" / "pasilla_fixture" / "counts.tsv"
    if not counts.exists():
        source = find_input_file(("count", "gene_counts"))
        if source is None:
            raise FileNotFoundError("counts input not registered in runtime/inputs.json")
        counts.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, counts)
    return read_counts(counts)


def render_required(task_id: str, spec: dict) -> dict:
    figs = required_figures(spec)
    if not figs:
        return {"figures": {}}
    sys.path.insert(0, str(PACKAGE))
    from runtime.plotting.core import generate

    stage_id = plot_stage(task_id, spec)
    mf = generate(stage_id=stage_id, outputs_dir=OUTPUTS / task_id, required=figs)
    missing = [f for f in figs if f not in mf.written]
    if missing:
        raise RuntimeError(
            f"required figure(s) not written for {task_id}: {missing}; "
            f"skipped={mf.skipped}; errors={mf.errors}"
        )
    return {
        "figures": {k: str(v) for k, v in mf.written.items()},
        "figure_formats": {k: [str(p) for p in v] for k, v in mf.formats.items()},
    }


def _bulk_rnaseq_data_acquisition(
    task_id: str, spec: dict, counts_src: Path, samples_src: Path
) -> dict:
    """Pasilla-style bulk-RNA-seq data acquisition: copy counts + sample
    sheet, derive per-sample stats from the counts matrix."""
    out = OUTPUTS / task_id
    gmt_src = find_input_file(("gmt", "gene_set"))
    data_dir = out / "data" / "pasilla_fixture"
    data_dir.mkdir(parents=True, exist_ok=True)
    counts_dst = data_dir / "counts.tsv"
    samples_dst = data_dir / "samples.tsv"
    shutil.copy2(counts_src, counts_dst)
    shutil.copy2(samples_src, samples_dst)
    if gmt_src is not None:
        shutil.copy2(gmt_src, data_dir / "gene_sets.gmt")
    genes, samples, matrix = read_counts(counts_dst)
    per_sample = {}
    for i, sample in enumerate(samples):
        values = [row[i] for row in matrix]
        per_sample[sample] = {
            "n_reads": int(sum(values)),
            "n_detected_genes": int(sum(1 for v in values if v > 0)),
        }
    write_json(
        out / "manifest.json",
        {
            "samples": [
                {"id": sample, "study_id": "pasilla_bioc", "size_bytes": counts_dst.stat().st_size}
                for sample in samples
            ],
            "files": [
                {"path": str(counts_dst), "sha256": sha256(counts_dst)},
                {"path": str(samples_dst), "sha256": sha256(samples_dst)},
            ],
        },
    )
    with (out / "cohort_manifest.tsv").open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["sample", "condition", "study_id"])
        for sample in samples:
            writer.writerow([sample, sample_condition(sample), "pasilla_bioc"])
    write_json(out / "matrices_index.json", {"counts": str(counts_dst), "samples": samples})
    write_json(
        out / "per_accession_summary.json",
        {"pasilla_bioc": {"n_samples": len(samples), "n_genes": len(genes)}},
    )
    rendered = render_required(task_id, spec)
    return {
        "status": "completed",
        "n_samples": len(samples),
        "n_genes": len(genes),
        "input_sha256": sha256(counts_dst),
        **rendered,
    }


# CSV/TSV columns that commonly hold sample identifiers across modalities.
SAMPLE_ID_COLUMNS = (
    "sample", "Sample", "sample_id", "Sample ID", "SampleID",
    "USUBJID", "SUBJID", "subject_id", "patient_id", "patient",
    "barcode", "cell_barcode", "specimen_id", "biosample",
    "id", "ID",
)


def _extract_samples_from_sheet(path: Path, study_id: str) -> list[dict]:
    """Best-effort sample-id extraction from a CSV/TSV sheet.

    Tries header column names first (any of SAMPLE_ID_COLUMNS), then falls
    back to using the first non-empty column. Returns one sample dict per
    data row.
    """
    samples: list[dict] = []
    try:
        with path.open(newline="") as f:
            head = f.read(8192)
            f.seek(0)
            try:
                dialect = csv.Sniffer().sniff(head, delimiters=",\t;|")
            except csv.Error:
                # Sniffer choked on a single-column file or unfamiliar layout.
                # Default to TSV which is the common case.
                dialect = csv.excel_tab
            reader = csv.reader(f, dialect=dialect)
            try:
                header = next(reader)
            except StopIteration:
                return samples
            # Find the sample-id column. Be tolerant of case + whitespace.
            sample_col_idx = None
            for idx, col in enumerate(header):
                normalized = col.strip()
                if normalized in SAMPLE_ID_COLUMNS or normalized.lower() in {
                    c.lower() for c in SAMPLE_ID_COLUMNS
                }:
                    sample_col_idx = idx
                    break
            if sample_col_idx is None and header:
                # Fallback: first column.
                sample_col_idx = 0
            if sample_col_idx is None:
                return samples
            for row in reader:
                if not row or len(row) <= sample_col_idx:
                    continue
                sid = row[sample_col_idx].strip()
                if not sid:
                    continue
                samples.append(
                    {"id": sid, "study_id": study_id, "size_bytes": path.stat().st_size}
                )
    except Exception:
        # Best-effort only — caller has fallbacks.
        return samples
    return samples


def _generic_data_acquisition(task_id: str, spec: dict) -> dict:
    """Generic data_acquisition fallback for any registered input shape.

    Writes manifest.json + cohort_manifest.tsv with a samples list derived
    from whatever is registered. Three-tier sample extraction:
      1. Read CSV/TSV sample sheets (ADSL/samples.csv/etc) for a column
         matching SAMPLE_ID_COLUMNS.
      2. Otherwise treat each registered file as one sample.
      3. Otherwise produce a single synthetic sample so the renderer doesn't
         zero-skip on an empty manifest.
    """
    out = OUTPUTS / task_id
    data_dir = out / "data" / "fixture"
    data_dir.mkdir(parents=True, exist_ok=True)

    inputs = load_inputs()
    copied_files: list[dict] = []
    samples: list[dict] = []
    sheet_extensions = {".csv", ".tsv", ".txt"}

    for entry in inputs:
        study_id = str(entry.get("input_id") or entry.get("label", "fixture_study")).split()[0]
        root = Path(str(entry.get("root_path", "")))
        for file_entry in entry.get("files", []):
            rel = file_entry.get("relpath") or file_entry.get("relative_path")
            if not rel:
                continue
            src = root / str(rel)
            if not src.exists():
                continue
            dst = data_dir / src.name
            try:
                shutil.copy2(src, dst)
            except OSError:
                # Skip files we can't copy (permissions/space) — keep going.
                continue
            sz = dst.stat().st_size
            copied_files.append(
                {"path": str(dst), "sha256": sha256(dst), "size_bytes": sz}
            )
            # Try sheet extraction first time we see a likely sample sheet.
            if not samples and src.suffix.lower() in sheet_extensions:
                sheet_samples = _extract_samples_from_sheet(dst, study_id)
                if sheet_samples:
                    samples = sheet_samples

    # Tier 2: one sample per registered file (excluding the sheet we already
    # parsed).
    if not samples and copied_files:
        for i, fobj in enumerate(copied_files):
            stem = Path(fobj["path"]).stem
            samples.append(
                {
                    "id": f"sample_{i}_{stem}",
                    "study_id": "fixture_cohort",
                    "size_bytes": fobj["size_bytes"],
                }
            )

    # Tier 3: synthetic placeholder so downstream renderers have something
    # to chew on rather than zero-skip the figure.
    if not samples:
        samples = [
            {"id": "fixture_sample_0", "study_id": "fixture_cohort", "size_bytes": 0}
        ]

    write_json(out / "manifest.json", {"samples": samples, "files": copied_files})
    with (out / "cohort_manifest.tsv").open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["sample", "condition", "study_id"])
        for s in samples:
            writer.writerow([s["id"], "fixture", s["study_id"]])
    # Group samples by study for the per-accession summary.
    studies: dict[str, int] = {}
    for s in samples:
        studies[s["study_id"]] = studies.get(s["study_id"], 0) + 1
    write_json(
        out / "per_accession_summary.json",
        {sid: {"n_samples": n} for sid, n in studies.items()},
    )
    write_json(out / "matrices_index.json", {"samples": [s["id"] for s in samples]})

    rendered = render_required(task_id, spec)
    return {
        "status": "completed",
        "n_samples": len(samples),
        "n_files": len(copied_files),
        "shape": "generic",
        **rendered,
    }


def data_acquisition(task_id: str, spec: dict) -> dict:
    """Dispatch to a shape-appropriate data_acquisition implementation.

    Preserves the pasilla-style bulk-RNA-seq path (counts + samples sheet)
    when both are registered; otherwise falls back to a generic handler
    that derives samples from any registered input.
    """
    counts_src = find_input_file(("count", "gene_counts"))
    samples_src = find_input_file(("sample", "annotation"))
    if counts_src is not None and samples_src is not None:
        return _bulk_rnaseq_data_acquisition(task_id, spec, counts_src, samples_src)
    return _generic_data_acquisition(task_id, spec)


def _write_quality_control_fixture(out: Path) -> None:
    per_sample = {
        "sample_A": {"n_reads": 12800, "n_detected_genes": 4300, "pct_mito": 2.1},
        "sample_B": {"n_reads": 11950, "n_detected_genes": 4100, "pct_mito": 2.9},
        "sample_C": {"n_reads": 15120, "n_detected_genes": 4650, "pct_mito": 1.8},
        "sample_D": {"n_reads": 9800, "n_detected_genes": 3800, "pct_mito": 3.4},
    }
    write_json(out / "manifest.json", {"per_sample_metrics": per_sample})
    with (out / "qc_metrics.tsv").open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["sample", "metric", "value"])
        for sample, metrics in per_sample.items():
            for metric, value in metrics.items():
                writer.writerow([sample, metric, value])
    write_json(
        out / "summary_stats.json",
        {
            "n_samples": len(per_sample),
            "median_detected_genes": 4200,
            "median_reads": 12375,
            "median_pct_mito": 2.5,
        },
    )


def _write_normalization_fixture(out: Path) -> None:
    samples = ["sample_A", "sample_B", "sample_C", "sample_D"]
    rows = []
    for i in range(1, 81):
        mean = 2.0 + i * 0.17
        variance = 0.4 + (i % 13) * 0.19 + mean * 0.08
        rows.append((f"gene_{i:03d}", mean, variance))
    with (out / "mean_variance.tsv").open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["feature", "mean", "variance"])
        for gene, mean, variance in rows:
            writer.writerow([gene, f"{mean:.4f}", f"{variance:.4f}"])
    with (out / "normalized_counts.tsv").open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["feature", *samples])
        for idx, (gene, mean, _variance) in enumerate(rows):
            values = [
                mean + math.sin(idx / 5.0) * 0.35,
                mean + math.cos(idx / 7.0) * 0.25,
                mean + 0.4 + math.sin(idx / 6.0) * 0.3,
                mean + 0.3 + math.cos(idx / 4.0) * 0.2,
            ]
            writer.writerow([gene, *[f"{v:.4f}" for v in values]])
    shutil.copy2(out / "normalized_counts.tsv", out / "vst_matrix.tsv")
    write_json(out / "manifest.json", {"runs": [{"id": "fixture_vst", "n_hvg": 32}]})


def _write_differential_expression_fixture(out: Path) -> None:
    rows = []
    for i in range(1, 91):
        sign = -1.0 if i % 2 else 1.0
        lfc = sign * (0.25 + (i % 17) * 0.11)
        base = 40.0 + i * 2.7
        pvalue = max(1e-8, min(0.95, math.exp(-abs(lfc) * 2.0) / (1 + i / 25.0)))
        padj = min(1.0, pvalue * 1.6)
        rows.append((f"gene_{i:03d}", base, lfc, pvalue, padj))
    with (out / "de_table.tsv").open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["gene", "baseMean", "log2FoldChange", "pvalue", "padj"])
        for gene, base, lfc, pvalue, padj in rows:
            writer.writerow(
                [gene, f"{base:.4f}", f"{lfc:.5f}", f"{pvalue:.8g}", f"{padj:.8g}"]
            )
    write_json(
        out / "manifest.json",
        {"comparisons": [{"id": "fixture_treated_vs_control", "table_path": "de_table.tsv"}]},
    )


def _write_enrichment_fixture(out: Path) -> None:
    enrichments = []
    for idx, term in enumerate(
        [
            "ribosome biogenesis",
            "RNA processing",
            "mitochondrial translation",
            "cell cycle",
            "stress response",
            "protein folding",
        ],
        start=1,
    ):
        p = 10 ** (-(idx + 1))
        enrichments.append(
            {
                "id": term.replace(" ", "_"),
                "term": term,
                "n_overlap": 5 + idx,
                "n_set": 30 + idx * 3,
                "n_universe": 120,
                "p_value": p,
                "adj_p_value": min(1.0, p * 6),
            }
        )
    write_json(out / "manifest.json", {"enrichments": enrichments})
    with (out / "enrichment.tsv").open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["term", "p_value", "adj_p_value", "n_overlap"])
        for row in enrichments:
            writer.writerow(
                [
                    row["term"],
                    f"{row['p_value']:.8g}",
                    f"{row['adj_p_value']:.8g}",
                    row["n_overlap"],
                ]
            )


def _write_tsv(path: Path, header: list[str], rows: list[list[object]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(header)
        writer.writerows(rows)


def _embedding_rows(n: int = 48, labels: tuple[str, ...] = ("A", "B", "C")) -> list[list[object]]:
    rows = []
    for i in range(n):
        label = labels[i % len(labels)]
        angle = i / 6.0
        radius = 1.0 + (i % 7) * 0.05
        x = math.cos(angle) * radius + (i % len(labels)) * 0.55
        y = math.sin(angle) * radius + (i % len(labels)) * 0.25
        rows.append([f"cell_{i:03d}", f"{x:.4f}", f"{y:.4f}", label])
    return rows


def _write_summary_stats(path: Path, n: int = 80) -> None:
    rows = []
    for i in range(1, n + 1):
        chrom = "1" if i <= n // 2 else "2"
        pos = 100_000 + i * 4_000
        pvalue = max(1e-12, min(0.95, 10 ** (-(1 + (i % 12) * 0.22))))
        rows.append([chrom, pos, f"{pvalue:.8g}", f"rs{i:04d}"])
    _write_tsv(path, ["chrom", "pos", "pvalue", "gene"], rows)


def _write_batch_correction_fixture(out: Path) -> None:
    write_json(
        out / "manifest.json",
        {"runs": [{"id": "integrated", "mixing_pre": 0.38, "mixing_post": 0.82}]},
    )
    write_json(out / "integration_stats.json", {"mixing_pre": 0.38, "mixing_post": 0.82})
    _write_tsv(
        out / "integrated_embeddings.tsv",
        ["cell", "umap_1", "umap_2", "batch"],
        _embedding_rows(labels=("batch_1", "batch_2", "batch_3")),
    )


def _write_vdj_repertoire_fixture(out: Path) -> None:
    vdj_rows = []
    chains = ("TRA", "TRB", "IGH", "IGK")
    segments = ("TRBV5-1", "TRBV7-2", "TRAV12-2", "IGHV3-23")
    for i in range(60):
        vdj_rows.append(
            [f"cell_{i:03d}", chains[i % len(chains)], 11 + (i % 9), segments[i % len(segments)]]
        )
    clonotype_rows = []
    for i in range(1, 25):
        freq = 0.18 / i
        clonotype_rows.append(
            [f"clonotype_{i:02d}", f"{freq:.5f}", "true" if i % 3 == 0 else "false", 4 + i]
        )
    _write_tsv(out / "vdj_per_cell.tsv", ["cell", "chain", "cdr3_length", "v_segment"], vdj_rows)
    _write_tsv(
        out / "clonotype_table.tsv",
        ["clonotype_id", "frequency", "is_public", "n_members"],
        clonotype_rows,
    )
    write_json(
        out / "manifest.json",
        {
            "vdj_per_cell_path": "vdj_per_cell.tsv",
            "clonotype_table_path": "clonotype_table.tsv",
        },
    )


def _write_cell_type_annotation_fixture(out: Path) -> None:
    labels = ("T cell", "B cell", "Monocyte")
    rows = _embedding_rows(labels=labels)
    _write_tsv(out / "celltype_labels.tsv", ["cell", "x", "y", "celltype"], rows)
    write_json(
        out / "manifest.json",
        {
            "runs": [
                {
                    "id": "cells",
                    "celltypes": [
                        {"id": label, "n_cells": sum(1 for row in rows if row[3] == label)}
                        for label in labels
                    ],
                }
            ]
        },
    )


def _write_chromatin_contacts_fixture(out: Path) -> None:
    rows = []
    for i in range(1, 40):
        chrom_a = "chr1" if i % 5 else "chr2"
        chrom_b = chrom_a if i % 4 else "chr2"
        rows.append([chrom_a, chrom_b, 10_000 + i * 2_500, 100 + (i % 11) * 12])
    _write_tsv(out / "contacts.tsv", ["chrom_a", "chrom_b", "distance", "count"], rows)
    write_json(out / "manifest.json", {"contacts_path": "contacts.tsv"})


def _write_chromatin_loops_fixture(out: Path) -> None:
    loop_rows = []
    diff_rows = []
    for i in range(1, 45):
        start = 100_000 + i * 8_000
        end = start + 15_000 + (i % 8) * 2_000
        size = end - start
        loop_rows.append([f"loop_{i:03d}", start, end, size])
        lfc = (-1 if i % 2 else 1) * (0.2 + (i % 9) * 0.16)
        pvalue = max(1e-8, 0.8 / (i + 3))
        diff_rows.append([f"loop_{i:03d}", start, end, f"{lfc:.4f}", f"{pvalue:.8g}"])
    _write_tsv(out / "loops.tsv", ["loop_id", "start", "end", "size"], loop_rows)
    _write_tsv(
        out / "differential_loops.tsv",
        ["loop_id", "start", "end", "log2fc", "pvalue"],
        diff_rows,
    )
    write_json(
        out / "manifest.json",
        {"loops_path": "loops.tsv", "differential_loops_path": "differential_loops.tsv"},
    )


def _write_regulatory_variants_fixture(out: Path) -> None:
    rows = []
    for i in range(1, 70):
        lfc = (-1 if i % 2 else 1) * (0.25 + (i % 13) * 0.09)
        pvalue = max(1e-9, 0.7 / (i + 4))
        rows.append([f"var_{i:03d}", f"{lfc:.5f}", f"{pvalue:.8g}", "true" if i % 3 else "false"])
    _write_tsv(out / "cis_scores.tsv", ["variant_id", "log2fc", "pvalue", "overlaps_peak"], rows)
    write_json(out / "manifest.json", {"scores_path": "cis_scores.tsv"})


def _write_clustering_fixture(out: Path) -> None:
    labels = ("0", "1", "2", "3")
    rows = _embedding_rows(n=72, labels=labels)
    _write_tsv(out / "cluster_labels.tsv", ["cell", "x", "y", "cluster"], rows)
    sizes = {label: sum(1 for row in rows if row[3] == label) for label in labels}
    write_json(out / "cluster_sizes.json", sizes)
    write_json(
        out / "manifest.json",
        {"runs": [{"id": "clusters", "clusters": [{"id": k, "n_cells": v} for k, v in sizes.items()]}]},
    )


def _write_colocalization_fixture(out: Path) -> None:
    _write_summary_stats(out / "summary_stats.tsv")
    _write_summary_stats(out / "miami_top.tsv")
    _write_summary_stats(out / "miami_bottom.tsv")
    _write_tsv(
        out / "credible_set.tsv",
        ["pos", "posterior", "credible_set"],
        [[100_000 + i * 3_000, f"{1.0 / (i + 3):.5f}", "cs1"] for i in range(1, 30)],
    )
    _write_tsv(
        out / "coloc.tsv",
        ["locus", "pp_h0", "pp_h1", "pp_h2", "pp_h3", "pp_h4"],
        [
            ["locus_1", "0.01", "0.04", "0.05", "0.10", "0.80"],
            ["locus_2", "0.03", "0.07", "0.08", "0.12", "0.70"],
            ["locus_3", "0.05", "0.10", "0.12", "0.18", "0.55"],
        ],
    )
    write_json(
        out / "manifest.json",
        {
            "summary_stats": "summary_stats.tsv",
            "miami_top": "miami_top.tsv",
            "miami_bottom": "miami_bottom.tsv",
            "credible_set_table": "credible_set.tsv",
            "coloc_table": "coloc.tsv",
        },
    )


def _write_differential_accessibility_fixture(out: Path) -> None:
    rows = []
    for i in range(1, 75):
        lfc = (-1 if i % 2 else 1) * (0.2 + (i % 15) * 0.11)
        pvalue = max(1e-9, 0.6 / (i + 4))
        rows.append([f"peak_{i:03d}", f"{lfc:.5f}", f"{pvalue:.8g}", f"{pvalue * 1.4:.8g}", 20 + i])
    _write_tsv(
        out / "differential_peaks.tsv",
        ["peak", "log2fc", "pvalue", "adj_pvalue", "baseMean"],
        rows,
    )
    write_json(out / "manifest.json", {"comparisons": [{"id": "case_vs_control", "table_path": "differential_peaks.tsv"}]})


def _write_dtu_fixture(out: Path) -> None:
    rows = []
    for gene_i in range(1, 9):
        for tx_i in range(1, 4):
            delta = (-1 if tx_i % 2 else 1) * (0.05 * tx_i + gene_i * 0.01)
            rows.append([f"gene_{gene_i:02d}", f"tx_{gene_i:02d}_{tx_i}", f"{delta:.4f}", f"{0.05 / (gene_i + tx_i):.8g}"])
    _write_tsv(out / "dtu.tsv", ["gene", "transcript", "dProportion", "pvalue"], rows)
    write_json(out / "manifest.json", {"dtu_path": "dtu.tsv"})


def _write_dimensionality_reduction_fixture(out: Path) -> None:
    _write_tsv(out / "embedding.tsv", ["x", "y"], [[row[1], row[2]] for row in _embedding_rows()])
    write_json(out / "variance_ratio.json", [0.32, 0.21, 0.13, 0.08, 0.05])
    write_json(
        out / "manifest.json",
        {"runs": [{"id": "pca", "variance_explained": [0.32, 0.21, 0.13, 0.08, 0.05], "embedding_path": "embedding.tsv"}]},
    )


def _write_taxonomic_profiling_fixture(out: Path) -> None:
    abundance = []
    for sample in ("sample_A", "sample_B", "sample_C"):
        for taxon, value in (("Bacteroides", 0.35), ("Faecalibacterium", 0.22), ("Roseburia", 0.15)):
            abundance.append([sample, taxon, value + (0.02 if sample == "sample_B" else 0.0)])
    diversity = [["case", 3.2], ["case", 3.5], ["control", 2.9], ["control", 3.1]]
    _write_tsv(out / "abundance.tsv", ["sample", "taxon", "abundance"], abundance)
    _write_tsv(out / "diversity.tsv", ["group", "value"], diversity)
    write_json(out / "manifest.json", {"abundance_table": "abundance.tsv", "diversity_table": "diversity.tsv"})


def _write_primary_endpoint_fixture(out: Path) -> None:
    _write_tsv(
        out / "survival.tsv",
        ["time", "event", "group"],
        [[3, 1, "control"], [5, 0, "control"], [4, 1, "treated"], [8, 0, "treated"], [10, 1, "treated"]],
    )
    _write_tsv(
        out / "forest.tsv",
        ["label", "effect", "ci_lo", "ci_hi", "weight"],
        [["overall", -0.35, -0.62, -0.08, 200], ["age_lt65", -0.28, -0.58, 0.02, 110], ["age_ge65", -0.44, -0.80, -0.08, 90]],
    )
    write_json(out / "manifest.json", {"survival_table": "survival.tsv", "forest_table": "forest.tsv"})


def _write_enhancer_activity_fixture(out: Path) -> None:
    rows = []
    for i in range(1, 50):
        dna = 50 + i * 7
        rna = dna * (0.6 + (i % 10) * 0.08)
        rows.append([f"enh_{i:03d}", dna, f"{rna:.3f}", f"{rna / dna:.4f}"])
    _write_tsv(out / "activity.tsv", ["enhancer", "dna_count", "rna_count", "activity_score"], rows)
    write_json(out / "manifest.json", {"activity_path": "activity.tsv"})


def _write_methylation_expression_fixture(out: Path) -> None:
    rows = []
    for i in range(1, 45):
        meth = 0.2 + (i % 20) * 0.03
        expr = 8.0 - meth * 3.2 + math.sin(i / 3.0)
        corr = -0.75 + (i % 7) * 0.08
        rows.append([f"gene_{i:03d}", f"{meth:.4f}", f"{expr:.4f}", f"{corr:.4f}"])
    _write_tsv(out / "correlations.tsv", ["gene", "methylation", "expression", "correlation"], rows)
    write_json(out / "manifest.json", {"correlations_path": "correlations.tsv"})


def _write_hla_peptidomics_fixture(out: Path) -> None:
    peptides = []
    binders = []
    neo = []
    alleles = ("HLA-A*02:01", "HLA-B*07:02", "HLA-C*07:01")
    for i in range(1, 45):
        pep = f"PEPTIDE{i:02d}"
        peptides.append([pep, 8 + (i % 5), f"{35 + i * 1.7:.3f}"])
        binders.append([pep, alleles[i % len(alleles)], f"{0.2 + (i % 12) * 0.18:.4f}"])
        neo.append([f"patient_{1 + i % 4}", pep])
    _write_tsv(out / "peptides.tsv", ["peptide", "length", "score"], peptides)
    _write_tsv(out / "binders.tsv", ["peptide", "allele", "rank"], binders)
    _write_tsv(out / "neoantigens.tsv", ["patient", "peptide"], neo)
    write_json(
        out / "manifest.json",
        {"peptides_path": "peptides.tsv", "binders_path": "binders.tsv", "neoantigens_path": "neoantigens.tsv"},
    )


def _write_multi_omics_integration_fixture(out: Path) -> None:
    write_json(
        out / "manifest.json",
        {
            "modality_concordance_matrix": [[1.0, 0.72, 0.55], [0.72, 1.0, 0.61], [0.55, 0.61, 1.0]],
            "row_labels": ["RNA", "ATAC", "Protein"],
            "col_labels": ["RNA", "ATAC", "Protein"],
            "factor_variance": [{"factor": "factor_1", "variance": 0.41}, {"factor": "factor_2", "variance": 0.27}],
        },
    )


def _write_joint_wnn_fixture(out: Path) -> None:
    rows = []
    for i, row in enumerate(_embedding_rows(labels=("0", "1", "2"))):
        rna = 0.35 + (i % 5) * 0.08
        atac = max(0.05, 1.0 - rna)
        rows.append([row[1], row[2], row[3], f"{rna:.4f}", f"{atac:.4f}"])
    _write_tsv(out / "wnn_embedding.tsv", ["x", "y", "cluster", "rna_weight", "atac_weight"], rows)
    write_json(out / "manifest.json", {"embedding_path": "wnn_embedding.tsv"})


def _write_isoform_calling_fixture(out: Path) -> None:
    _write_tsv(
        out / "isoforms.tsv",
        ["transcript", "exon_starts", "exon_ends", "strand"],
        [["tx1", "100,300,520", "180,420,700", "+"], ["tx2", "120,360", "220,650", "+"], ["tx3", "90,260,480", "160,380,620", "-"]],
    )
    _write_tsv(out / "junctions.tsv", ["junction", "count"], [["180-300", 42], ["220-360", 35], ["420-520", 28], ["380-480", 22]])
    write_json(out / "manifest.json", {"isoform_table": "isoforms.tsv", "junction_table": "junctions.tsv"})


def _write_motif_enrichment_fixture(out: Path) -> None:
    motifs = [
        ["MA0001", "bZIP", "1e-8", "3.2", "ATGCAA"],
        ["MA0002", "ETS", "2e-7", "2.6", "GGAACT"],
        ["MA0003", "GATA", "5e-6", "2.1", "GATAAG"],
        ["MA0004", "RUNX", "7e-5", "1.9", "TGTGGT"],
    ]
    _write_tsv(out / "enriched_motifs.tsv", ["motif_id", "family", "adj_p_value", "fold_enrichment", "consensus"], motifs)
    write_json(out / "manifest.json", {"motif_table": "enriched_motifs.tsv"})


def _write_barcode_pairing_fixture(out: Path) -> None:
    write_json(
        out / "pairing_summary.json",
        {
            "flows": [
                {"source": "RNA barcodes", "target": "matched nuclei", "n": 840},
                {"source": "ATAC barcodes", "target": "matched nuclei", "n": 815},
                {"source": "matched nuclei", "target": "high confidence", "n": 790},
            ]
        },
    )
    write_json(out / "manifest.json", {"summary_path": "pairing_summary.json"})


def _write_peak_annotation_fixture(out: Path) -> None:
    classes = ("promoter", "enhancer", "intron", "intergenic")
    rows = [[f"peak_{i:03d}", classes[i % len(classes)], (-1) ** i * (200 + i * 850)] for i in range(1, 60)]
    _write_tsv(out / "peak_annotation.tsv", ["peak", "feature_class", "distance_to_tss"], rows)
    write_json(out / "manifest.json", {"annotation_table": "peak_annotation.tsv"})


def _write_peak_calling_fixture(out: Path) -> None:
    _write_tsv(out / "profile.tsv", ["position", "signal", "group"], [[i * 50 - 2500, 10 + 30 * math.exp(-(i - 50) ** 2 / 800), "IP"] for i in range(101)])
    _write_tsv(out / "coverage.tsv", ["chrom", "pos", "depth"], [["chr1", 100_000 + i * 100, 20 + (i % 17)] for i in range(100)])
    _write_tsv(out / "saturation.tsv", ["depth", "peaks_called", "group"], [[i * 1_000, 1000 * (1 - math.exp(-i / 12)), "rep1"] for i in range(1, 40)])
    write_json(out / "manifest.json", {"profile_table": "profile.tsv", "coverage_table": "coverage.tsv", "saturation_table": "saturation.tsv"})


def _write_peak_to_gene_fixture(out: Path) -> None:
    rows = []
    for i in range(1, 45):
        start = 100_000 + i * 5_000
        rows.append([f"peak_{i:03d}", f"gene_{i % 10}", start, start + 20_000 + (i % 5) * 4_000, f"{0.2 + (i % 9) * 0.07:.4f}", f"cluster_{i % 3}"])
    _write_tsv(out / "links.tsv", ["peak", "gene", "peak_start", "tss", "score", "cluster"], rows)
    write_json(out / "manifest.json", {"links_path": "links.tsv"})


def _write_population_definition_fixture(out: Path) -> None:
    write_json(
        out / "manifest.json",
        {"population_flow": {"enrolled": 240, "randomized": 220, "allocated": 216, "followed_up": 204, "analyzed": 198}},
    )


def _write_quantification_fixture(out: Path) -> None:
    _write_tsv(out / "coverage.tsv", ["position", "coverage"], [[i, 1 + (i % 8)] for i in range(1, 120)])
    rows = []
    for sample in ("sample_A", "sample_B", "sample_C"):
        for i in range(36):
            rows.append([sample, f"{20.0 + (i % 12) * 0.35 + (0.4 if sample == 'sample_B' else 0.0):.4f}"])
    _write_tsv(out / "intensity.tsv", ["group", "value"], rows)
    write_json(out / "manifest.json", {"coverage_table": "coverage.tsv", "intensity_table": "intensity.tsv"})


def _write_ribo_seq_fixture(out: Path) -> None:
    _write_tsv(out / "psite_offset.tsv", ["read_length", "offset"], [[length, 12 + (length % 5)] for length in range(26, 35)])
    _write_tsv(out / "frame_counts.tsv", ["frame", "count"], [["0", 1800], ["1", 420], ["2", 390]])
    rows = []
    for i in range(1, 60):
        rna = (-1 if i % 2 else 1) * (0.1 + (i % 7) * 0.08)
        ribo = rna + (0.15 if i % 3 else -0.12)
        te = ribo - rna
        rows.append([f"gene_{i:03d}", f"{te:.4f}", f"{rna:.4f}", f"{ribo:.4f}", f"{0.08 / (i + 2):.8g}"])
    _write_tsv(out / "te_results.tsv", ["gene", "log2fc_te", "log2fc_rna", "log2fc_ribo", "pvalue"], rows)
    write_json(out / "manifest.json", {"psite_offset_path": "psite_offset.tsv", "frame_counts_path": "frame_counts.tsv", "te_results_path": "te_results.tsv"})


def _write_preprocessing_fixture(out: Path) -> None:
    write_json(
        out / "manifest.json",
        {
            "samples": [
                {"id": "sample_A", "n_in": 10000, "n_out": 9200},
                {"id": "sample_B", "n_in": 9800, "n_out": 8700},
                {"id": "sample_C", "n_in": 10500, "n_out": 9400},
            ]
        },
    )


def _write_crispr_screen_fixture(out: Path) -> None:
    rows = []
    for i in range(80):
        rows.append([f"cell_{i:03d}", f"sg{i % 8}", f"gene_{i % 5}", 1 + (i % 18)])
    _write_tsv(out / "sgrna_assignments.tsv", ["cell", "sgrna", "perturbation", "umi_count"], rows)
    write_json(out / "manifest.json", {"assignments_path": "sgrna_assignments.tsv"})


def _write_spatial_clustering_fixture(out: Path) -> None:
    coords = [[i % 12, i // 12, i % 4] for i in range(72)]
    morans = [[f"gene_{i:03d}", f"{0.15 + (i % 20) * 0.025:.4f}", f"{0.05 / (i + 2):.8g}"] for i in range(1, 45)]
    neighborhoods = [[f"domain_{i}", f"domain_{j}", f"{(i + 1) * (j + 2) / 10:.4f}"] for i in range(3) for j in range(3)]
    _write_tsv(out / "coords.tsv", ["x", "y", "value"], coords)
    _write_tsv(out / "morans_i.tsv", ["gene", "morans_i", "p_value"], morans)
    _write_tsv(out / "neighborhood.tsv", ["source", "target", "score"], neighborhoods)
    write_json(out / "manifest.json", {"coords_table": "coords.tsv", "morans_i_table": "morans_i.tsv", "neighborhood_table": "neighborhood.tsv"})


def _write_forecasting_inference_fixture(out: Path) -> None:
    forecast = []
    anomaly = []
    for i in range(1, 32):
        actual = 100 + i * 1.5 + math.sin(i / 3.0) * 8
        pred = 100 + i * 1.45
        forecast.append([i, f"{pred:.3f}", f"{pred - 8:.3f}", f"{pred + 8:.3f}", f"{actual:.3f}", "series_A"])
        anomaly.append([i, f"{actual:.3f}", "true" if i in {12, 24} else "false"])
    _write_tsv(out / "forecast.tsv", ["time", "forecast", "lower", "upper", "actual", "group"], forecast)
    _write_tsv(out / "anomaly.tsv", ["time", "value", "is_anomaly"], anomaly)
    write_json(out / "manifest.json", {"forecast_table": "forecast.tsv", "anomaly_table": "anomaly.tsv"})


def _write_exploratory_analysis_fixture(out: Path) -> None:
    rows = [[i, f"{100 + math.sin(i / 3.0) * 10 + i * 0.4:.4f}"] for i in range(1, 60)]
    _write_tsv(out / "series.tsv", ["time", "value"], rows)
    write_json(out / "manifest.json", {"series_table": "series.tsv"})


def _write_variant_filtering_fixture(out: Path) -> None:
    _write_summary_stats(out / "summary_stats.tsv")
    write_json(out / "manifest.json", {"summary_stats": "summary_stats.tsv"})


def _write_stage_fixture(task_id: str, spec: dict, note: str) -> None:
    out = OUTPUTS / task_id
    stage_id = plot_stage(task_id, spec)
    if stage_id == "barcode_pairing":
        _write_barcode_pairing_fixture(out)
    elif stage_id == "batch_correction":
        _write_batch_correction_fixture(out)
    elif stage_id == "cell_type_annotation":
        _write_cell_type_annotation_fixture(out)
    elif stage_id == "chromatin_contacts":
        _write_chromatin_contacts_fixture(out)
    elif stage_id == "chromatin_loops":
        _write_chromatin_loops_fixture(out)
    elif stage_id == "clustering":
        _write_clustering_fixture(out)
    elif stage_id == "colocalization":
        _write_colocalization_fixture(out)
    elif stage_id == "crispr_screen":
        _write_crispr_screen_fixture(out)
    elif stage_id == "differential_accessibility":
        _write_differential_accessibility_fixture(out)
    elif stage_id == "dimensionality_reduction":
        _write_dimensionality_reduction_fixture(out)
    elif stage_id == "dtu":
        _write_dtu_fixture(out)
    elif stage_id == "enhancer_activity":
        _write_enhancer_activity_fixture(out)
    elif stage_id == "exploratory_analysis":
        _write_exploratory_analysis_fixture(out)
    elif stage_id == "forecasting_inference":
        _write_forecasting_inference_fixture(out)
    elif stage_id == "hla_peptidomics":
        _write_hla_peptidomics_fixture(out)
    elif stage_id == "isoform_calling":
        _write_isoform_calling_fixture(out)
    elif stage_id == "joint_wnn":
        _write_joint_wnn_fixture(out)
    elif stage_id == "methylation_expression":
        _write_methylation_expression_fixture(out)
    elif stage_id == "motif_enrichment":
        _write_motif_enrichment_fixture(out)
    elif stage_id == "multi_omics_integration":
        _write_multi_omics_integration_fixture(out)
    elif stage_id == "peak_annotation":
        _write_peak_annotation_fixture(out)
    elif stage_id == "peak_calling":
        _write_peak_calling_fixture(out)
    elif stage_id == "peak_to_gene":
        _write_peak_to_gene_fixture(out)
    elif stage_id == "population_definition":
        _write_population_definition_fixture(out)
    elif stage_id == "preprocessing":
        _write_preprocessing_fixture(out)
    elif stage_id == "primary_endpoint":
        _write_primary_endpoint_fixture(out)
    elif stage_id == "quality_control":
        _write_quality_control_fixture(out)
    elif stage_id == "quantification":
        _write_quantification_fixture(out)
    elif stage_id == "regulatory_variants":
        _write_regulatory_variants_fixture(out)
    elif stage_id == "ribo_seq":
        _write_ribo_seq_fixture(out)
    elif stage_id == "spatial_clustering":
        _write_spatial_clustering_fixture(out)
    elif stage_id == "taxonomic_profiling":
        _write_taxonomic_profiling_fixture(out)
    elif stage_id == "variant_filtering":
        _write_variant_filtering_fixture(out)
    elif stage_id == "vdj_repertoire":
        _write_vdj_repertoire_fixture(out)
    elif stage_id == "normalization":
        _write_normalization_fixture(out)
    elif stage_id == "differential_expression":
        _write_differential_expression_fixture(out)
    elif stage_id == "biological_interpretation":
        _write_enrichment_fixture(out)
    else:
        write_json(out / "manifest.json", {"note": note})


def _generic_completion(task_id: str, spec: dict, note: str) -> dict:
    """Complete a task with deterministic fixture artifacts.

    If the task declares required figures, this path writes the minimal
    stage-shaped inputs needed by the local renderer and then requires all
    requested figures to render. This keeps UI-driven smoke workflows honest:
    a task with plot affordances cannot silently succeed with no plots just
    because the user did not register concrete local input files.
    """
    out = OUTPUTS / task_id
    out.mkdir(parents=True, exist_ok=True)
    _write_stage_fixture(task_id, spec, note)
    rendered = render_required(task_id, spec) if required_figures(spec) else {"figures": {}}
    return {"status": "completed", "shape": "generic", "note": note, **rendered}


def qc_preprocessing(task_id: str, spec: dict) -> dict:
    out = OUTPUTS / task_id
    try:
        genes, samples, matrix = ensure_data_acquisition()
    except FileNotFoundError:
        return _generic_completion(
            task_id, spec, "no pasilla counts registered — generic completion"
        )
    per_sample = {}
    for i, sample in enumerate(samples):
        values = [row[i] for row in matrix]
        per_sample[sample] = {
            "n_reads": int(sum(values)),
            "n_detected_genes": int(sum(1 for v in values if v > 0)),
        }
    write_json(out / "manifest.json", {"per_sample_metrics": per_sample})
    with (out / "qc_metrics.tsv").open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["sample", "metric", "value"])
        for sample, metrics in per_sample.items():
            for metric, value in metrics.items():
                writer.writerow([sample, metric, value])
    write_json(
        out / "summary_stats.json",
        {
            "n_samples": len(samples),
            "n_genes": len(genes),
            "median_detected_genes": sorted(m["n_detected_genes"] for m in per_sample.values())[
                len(per_sample) // 2
            ],
        },
    )
    rendered = render_required(task_id, spec)
    return {"status": "completed", "n_samples": len(samples), **rendered}


def normalisation(task_id: str, spec: dict) -> dict:
    out = OUTPUTS / task_id
    try:
        genes, samples, matrix = ensure_data_acquisition()
    except FileNotFoundError:
        return _generic_completion(
            task_id, spec, "no pasilla counts registered — generic completion"
        )
    lib_sizes = [sum(row[i] for row in matrix) for i in range(len(samples))]
    norm_rows: list[list[float]] = []
    mean_var: list[tuple[str, float, float]] = []
    for gene, row in zip(genes, matrix):
        norm = [math.log2((value / max(lib_sizes[i], 1.0)) * 1_000_000 + 1.0) for i, value in enumerate(row)]
        norm_rows.append(norm)
        mean = sum(norm) / len(norm)
        var = sum((v - mean) ** 2 for v in norm) / max(len(norm) - 1, 1)
        mean_var.append((gene, mean, var))
    with (out / "normalized_counts.tsv").open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["feature", *samples])
        for gene, norm in zip(genes, norm_rows):
            writer.writerow([gene, *[f"{v:.6f}" for v in norm]])
    shutil.copy2(out / "normalized_counts.tsv", out / "vst_matrix.tsv")
    with (out / "mean_variance.tsv").open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["feature", "mean", "variance"])
        for gene, mean, var in mean_var:
            writer.writerow([gene, f"{mean:.6f}", f"{var:.6f}"])
    variances = sorted(v for _, _, v in mean_var)
    hvg_threshold = variances[len(variances) // 2] if variances else 0.0
    n_hvg = sum(1 for _gene, _mean, var in mean_var if var >= hvg_threshold)
    write_json(out / "manifest.json", {"runs": [{"id": "pasilla_vst", "n_hvg": n_hvg}]})
    rendered = render_required(task_id, spec)
    return {"status": "completed", "n_features": len(genes), "n_hvg": n_hvg, **rendered}


def differential_expression(task_id: str, spec: dict) -> dict:
    out = OUTPUTS / task_id
    try:
        genes, samples, matrix = ensure_data_acquisition()
    except FileNotFoundError:
        return _generic_completion(
            task_id, spec, "no pasilla counts registered — generic completion"
        )
    treated_idx = [i for i, sample in enumerate(samples) if sample_condition(sample) == "treated"]
    untreated_idx = [i for i, sample in enumerate(samples) if sample_condition(sample) == "untreated"]
    if not treated_idx or not untreated_idx:
        return _generic_completion(
            task_id, spec, "no treated/untreated split available — generic completion"
        )
    rows = []
    for gene, values in zip(genes, matrix):
        treated_mean = sum(values[i] for i in treated_idx) / len(treated_idx)
        untreated_mean = sum(values[i] for i in untreated_idx) / len(untreated_idx)
        base_mean = (treated_mean + untreated_mean) / 2.0
        log2fc = math.log2((treated_mean + 1.0) / (untreated_mean + 1.0))
        score = abs(log2fc) * math.log10(base_mean + 2.0)
        pvalue = max(1e-12, min(1.0, math.exp(-score)))
        rows.append((gene, base_mean, log2fc, pvalue))
    rows.sort(key=lambda r: r[3])
    n = len(rows)
    adjusted = []
    running = 1.0
    for rank_from_end, row in enumerate(reversed(rows), start=1):
        rank = n - rank_from_end + 1
        running = min(running, row[3] * n / max(rank, 1))
        adjusted.append((row, min(running, 1.0)))
    adjusted = list(reversed(adjusted))
    table = out / "de_table.tsv"
    with table.open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["gene", "baseMean", "log2FoldChange", "pvalue", "padj"])
        for row, padj in adjusted:
            gene, base_mean, log2fc, pvalue = row
            writer.writerow([gene, f"{base_mean:.6f}", f"{log2fc:.6f}", f"{pvalue:.8g}", f"{padj:.8g}"])
    significant = sum(1 for row, padj in adjusted if abs(row[2]) >= 1.0 and padj <= 0.05)
    write_json(
        out / "manifest.json",
        {"comparisons": [{"id": "treated_vs_untreated", "table_path": "de_table.tsv"}]},
    )
    rendered = render_required(task_id, spec)
    return {"status": "completed", "n_features": len(rows), "n_significant": significant, **rendered}


def pathway_enrichment(task_id: str, spec: dict) -> dict:
    out = OUTPUTS / task_id
    de_path = OUTPUTS / "differential_expression" / "de_table.tsv"
    if not de_path.exists():
        return _generic_completion(
            task_id, spec, "upstream DE table absent — generic completion"
        )
    gmt_path = OUTPUTS / "data_acquisition" / "data" / "pasilla_fixture" / "gene_sets.gmt"
    gene_sets = read_gene_sets(gmt_path)
    if not gene_sets:
        return _generic_completion(
            task_id, spec, "no gene sets registered — generic completion"
        )

    ranked: list[tuple[str, float, float]] = []
    with de_path.open(newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        for row in reader:
            gene = row.get("gene") or ""
            if not gene:
                continue
            try:
                lfc = abs(float(row.get("log2FoldChange") or 0.0))
                padj = float(row.get("padj") or 1.0)
            except ValueError:
                continue
            ranked.append((gene, lfc, padj))
    ranked.sort(key=lambda row: (-row[1], row[2], row[0]))
    foreground = {gene for gene, _lfc, _padj in ranked[: max(10, min(80, len(ranked) // 20))]}
    universe = {gene for gene, _lfc, _padj in ranked}
    enrichments = []
    for idx, (term, genes) in enumerate(sorted(gene_sets.items())):
        overlap = sorted(foreground & genes)
        if not overlap:
            continue
        # Deterministic enrichment score for fixture validation. It is not a
        # statistical test; it just produces ordered, plausible pathway rows.
        ratio = len(overlap) / max(len(genes & universe), 1)
        p_value = max(1e-8, min(1.0, (1.0 - min(ratio, 0.95)) / (idx + 2)))
        enrichments.append(
            {
                "id": term,
                "term": term,
                "n_overlap": len(overlap),
                "n_set": len(genes & universe),
                "n_universe": len(universe),
                "p_value": p_value,
                "adj_p_value": min(1.0, p_value * max(len(gene_sets), 1)),
                "genes": overlap,
            }
        )
    if not enrichments:
        raise FileNotFoundError("no foreground genes overlapped fixture gene sets")
    enrichments.sort(key=lambda row: (row["adj_p_value"], row["term"]))

    write_json(out / "manifest.json", {"enrichments": enrichments})
    with (out / "enrichment.tsv").open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["term", "p_value", "adj_p_value", "n_overlap"])
        for row in enrichments:
            writer.writerow(
                [
                    row["term"],
                    f"{row['p_value']:.8g}",
                    f"{row['adj_p_value']:.8g}",
                    row["n_overlap"],
                ]
            )
    rendered = render_required(task_id, spec)
    return {"status": "completed", "n_enriched_terms": len(enrichments), **rendered}


def reporting(task_id: str, spec: dict) -> dict:
    out = OUTPUTS / task_id
    write_json(
        out / "manifest.json",
        {
            "concordance_matrix": [[1.0, 0.82, 0.74], [0.82, 1.0, 0.68], [0.74, 0.68, 1.0]],
            "row_labels": ["QC", "normalisation", "DE"],
            "col_labels": ["QC", "normalisation", "DE"],
            "pathway_overlap": [
                {"label": "pasilla_treated_up_signature", "count": 18},
                {"label": "pasilla_treated_down_signature", "count": 15},
                {"label": "balanced_background_signature", "count": 11},
            ],
        },
    )
    rendered = render_required(task_id, spec)
    return {"status": "completed", **rendered}


def final_reporting(task_id: str, spec: dict) -> dict:
    out = OUTPUTS / task_id
    upstream = []
    for stage_dir in sorted(OUTPUTS.iterdir()):
        if stage_dir.name == task_id or not stage_dir.is_dir():
            continue
        manifest = stage_dir / "figures" / "manifest.json"
        if manifest.exists():
            data = load_json(manifest, {}) or {}
            upstream.append(
                {
                    "stage_id": stage_dir.name,
                    "figures": [
                        {"id": fig_id, "path": path}
                        for fig_id, path in sorted((data.get("written") or {}).items())
                    ],
                }
            )
    write_json(out / "manifest.json", {"upstream": upstream})
    rendered = render_required(task_id, spec)
    return {"status": "completed", "upstream_stage_count": len(upstream), **rendered}


def discovery(task_id: str) -> dict:
    out = OUTPUTS / task_id
    method = "fixture_vst" if "normal" in task_id else "fixture_pasilla_ranked_de"
    decision = {
        "task_id": task_id,
        "top_candidate": method,
        "runner_ups": [],
        "scores": {method: 1.0},
        "rationale": "Deterministic fixture method for local harness and plotting validation.",
        "auto_picked": True,
    }
    write_json(out / "decision.json", decision)
    return {"status": "completed", "top_candidate": method, "decision": decision}


def validation(task_id: str) -> dict:
    out = OUTPUTS / task_id
    target = task_id.replace("validate_", "", 1)
    target_dir = OUTPUTS / target
    checks = {
        "target_result_present": (target_dir / "result.json").exists(),
        "target_patch_present": (target_dir / "state.patch.applied.json").exists()
        or (target_dir / "state.patch.json").exists(),
    }
    fig_manifest = target_dir / "figures" / "manifest.json"
    if fig_manifest.exists():
        data = load_json(fig_manifest, {}) or {}
        checks["figures_present"] = bool(data.get("written"))
    passed = all(checks.values())
    write_json(out / "validation_report.json", {"target_task_id": target, "checks": checks, "passed": passed})
    if not passed:
        raise RuntimeError(f"validation failed for {target}: {checks}")
    return {"status": "completed", "target_task_id": target, "checks": checks}


def _read_cdisc_csv(path: Path) -> list[dict]:
    """Read a CDISC ADaM-shaped CSV into a list of row dicts. Best-effort
    only — returns [] on any parse error so handlers can fall through."""
    if not path or not path.exists():
        return []
    try:
        with path.open(newline="") as f:
            reader = csv.DictReader(f)
            return list(reader)
    except Exception:
        return []


def _adsl_subjects(adsl: Path) -> list[dict]:
    rows = _read_cdisc_csv(adsl)
    out = []
    for r in rows:
        sid = r.get("USUBJID") or r.get("SUBJID") or r.get("id")
        if not sid:
            continue
        out.append(
            {
                "USUBJID": sid,
                "ARM": (r.get("ARM") or "UNKNOWN").upper(),
                "AGE": r.get("AGE", ""),
                "SEX": r.get("SEX", ""),
            }
        )
    return out


def clinical_endpoint_analysis(task_id: str, spec: dict) -> dict:
    """Synthesize a survival_table + forest_table from ADSL/ADAE so the
    primary_endpoint renderer can produce kaplan_meier + forest figures.

    Real implementations consume the SAP-prespecified time-to-event +
    covariate columns; this fixture derives a minimal valid table from
    the registered CDISC ADaM inputs so the harness ordering and plot
    pipeline are exercised end-to-end without an LLM agent or a live
    statistical analysis."""
    out = OUTPUTS / task_id
    out.mkdir(parents=True, exist_ok=True)
    adsl = find_input_file(("adsl",))
    adae = find_input_file(("adae",))

    subjects = _adsl_subjects(adsl) if adsl else []
    # Derive a synthetic time-to-event: AE start day if the subject has
    # any AE in ADAE, otherwise a fixed censoring time. Event indicator
    # is 1 (event) when an AE is present, 0 (censored) otherwise. This
    # is NOT clinically meaningful — it's a minimum-shape table so the
    # renderer's column-extractor finds something to draw.
    ae_first_day: dict[str, int] = {}
    for r in _read_cdisc_csv(adae) if adae else []:
        sid = r.get("USUBJID")
        if not sid:
            continue
        try:
            day = int(float(r.get("AESTDY", "0") or "0"))
        except ValueError:
            day = 0
        if sid not in ae_first_day or day < ae_first_day[sid]:
            ae_first_day[sid] = day

    survival_rows: list[list[str]] = [["USUBJID", "time", "event", "group"]]
    for s in subjects:
        sid = s["USUBJID"]
        time = ae_first_day.get(sid, 730)  # 24-month censoring default
        event = 1 if sid in ae_first_day else 0
        survival_rows.append([sid, str(time), str(event), s["ARM"]])
    survival_path = out / "survival_table.tsv"
    with survival_path.open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerows(survival_rows)

    # Forest table: one row per ARM contrast (treatment vs placebo).
    # Effect = log hazard ratio (synthetic, just to populate the column).
    by_arm: dict[str, list[dict]] = {}
    for s in subjects:
        by_arm.setdefault(s["ARM"], []).append(s)
    forest_rows = [["label", "effect", "ci_lo", "ci_hi", "weight"]]
    placebo_n = len(by_arm.get("PLACEBO", []))
    for arm, members in sorted(by_arm.items()):
        if arm == "PLACEBO":
            continue
        n = len(members)
        forest_rows.append([f"{arm} vs PLACEBO", "-0.35", "-0.62", "-0.08", str(n + placebo_n)])
    if len(forest_rows) == 1:
        # Single-arm fallback so the renderer doesn't error.
        forest_rows.append(["Overall", "0.0", "-0.10", "0.10", str(len(subjects) or 1)])
    forest_path = out / "forest_table.tsv"
    with forest_path.open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerows(forest_rows)

    write_json(
        out / "manifest.json",
        {
            "survival_table": str(survival_path),
            "forest_table": str(forest_path),
            "n_subjects": len(subjects),
            "arms": sorted(by_arm.keys()),
        },
    )
    rendered = render_required(task_id, spec)
    return {"status": "completed", "n_subjects": len(subjects), **rendered}


def clinical_safety_summary(task_id: str, spec: dict) -> dict:
    """Synthesize ae_table + cumulative_incidence_table from ADAE."""
    out = OUTPUTS / task_id
    out.mkdir(parents=True, exist_ok=True)
    adae = find_input_file(("adae",))

    rows = _read_cdisc_csv(adae) if adae else []
    by_term: dict[tuple[str, str], int] = {}
    cumulative: list[list[str]] = [["USUBJID", "time", "event", "group"]]
    for r in rows:
        term = (r.get("AETERM") or "Unknown").strip()
        severity = (r.get("AESEV") or "MILD").upper()
        by_term[(term, severity)] = by_term.get((term, severity), 0) + 1
        sid = r.get("USUBJID") or "?"
        try:
            day = int(float(r.get("AESTDY", "0") or "0"))
        except ValueError:
            day = 0
        cumulative.append([sid, str(day), "1", "ALL"])
    if len(cumulative) == 1:
        cumulative.append(["S0", "180", "0", "ALL"])

    ae_path = out / "ae_table.tsv"
    with ae_path.open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["term", "count", "severity"])
        for (term, sev), count in sorted(by_term.items()):
            writer.writerow([term, str(count), sev])
        if not by_term:
            writer.writerow(["No AEs", "0", "MILD"])

    ci_path = out / "cumulative_incidence_table.tsv"
    with ci_path.open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerows(cumulative)

    write_json(
        out / "manifest.json",
        {
            "ae_table": str(ae_path),
            "cumulative_incidence_table": str(ci_path),
            "n_events": sum(by_term.values()),
        },
    )
    rendered = render_required(task_id, spec)
    return {"status": "completed", "n_events": sum(by_term.values()), **rendered}


def clinical_subgroup_analysis(task_id: str, spec: dict) -> dict:
    """Synthesize subgroup_forest table from ADSL strata (age, sex)."""
    out = OUTPUTS / task_id
    out.mkdir(parents=True, exist_ok=True)
    adsl = find_input_file(("adsl",))
    subjects = _adsl_subjects(adsl) if adsl else []

    forest_rows = [["label", "effect", "ci_lo", "ci_hi", "weight"]]
    # Age bands
    young = [s for s in subjects if s["AGE"] and int(float(s["AGE"])) < 50]
    older = [s for s in subjects if s["AGE"] and int(float(s["AGE"])) >= 50]
    if young:
        forest_rows.append(["Age <50", "-0.28", "-0.55", "0.00", str(len(young))])
    if older:
        forest_rows.append(["Age >=50", "-0.42", "-0.70", "-0.14", str(len(older))])
    # Sex
    male = [s for s in subjects if s["SEX"] == "M"]
    female = [s for s in subjects if s["SEX"] == "F"]
    if male:
        forest_rows.append(["Sex M", "-0.30", "-0.60", "0.00", str(len(male))])
    if female:
        forest_rows.append(["Sex F", "-0.40", "-0.65", "-0.15", str(len(female))])
    if len(forest_rows) == 1:
        forest_rows.append(["Overall", "0.0", "-0.10", "0.10", "1"])
    forest_path = out / "subgroup_forest_table.tsv"
    with forest_path.open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerows(forest_rows)

    write_json(
        out / "manifest.json",
        {"forest_table": str(forest_path), "n_subgroups": len(forest_rows) - 1},
    )
    rendered = render_required(task_id, spec)
    return {"status": "completed", "n_subgroups": len(forest_rows) - 1, **rendered}


def clinical_sensitivity_analysis(task_id: str, spec: dict) -> dict:
    """Synthesize secondary-endpoint-style tables (spaghetti + forest)
    from ADLB. Per-subject longitudinal values become spaghetti lines."""
    out = OUTPUTS / task_id
    out.mkdir(parents=True, exist_ok=True)
    adlb = find_input_file(("adlb",))
    rows = _read_cdisc_csv(adlb) if adlb else []

    # Spaghetti table: USUBJID, AVISIT (encoded as time), AVAL (value), PARAM
    spaghetti_rows = [["subject", "time", "value", "param"]]
    visit_to_time = {"Baseline": 0, "Week 12": 12, "Week 24": 24}
    for r in rows:
        sid = r.get("USUBJID")
        param = r.get("PARAM") or "PARAM"
        avisit = r.get("AVISIT") or "Baseline"
        try:
            aval = float(r.get("AVAL", "0") or "0")
        except ValueError:
            aval = 0.0
        t = visit_to_time.get(avisit, len(visit_to_time))
        if sid:
            spaghetti_rows.append([sid, str(t), str(aval), param])
    if len(spaghetti_rows) == 1:
        spaghetti_rows.append(["S0", "0", "0", "PARAM"])
    spaghetti_path = out / "longitudinal_table.tsv"
    with spaghetti_path.open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerows(spaghetti_rows)

    # Sensitivity forest: one row per analysis variant (per-protocol,
    # complete-case, multiple-imputation, etc.).
    sens_path = out / "sensitivity_forest_table.tsv"
    with sens_path.open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t")
        writer.writerow(["label", "effect", "ci_lo", "ci_hi", "weight"])
        writer.writerow(["Primary (ITT)", "-0.35", "-0.62", "-0.08", "200"])
        writer.writerow(["Per-protocol", "-0.40", "-0.68", "-0.12", "180"])
        writer.writerow(["Complete cases", "-0.32", "-0.60", "-0.04", "170"])
        writer.writerow(["MI (m=10)", "-0.36", "-0.63", "-0.09", "200"])

    write_json(
        out / "manifest.json",
        {
            "longitudinal_table": str(spaghetti_path),
            "spaghetti_table": str(spaghetti_path),
            "forest_table": str(sens_path),
            "n_observations": len(rows),
        },
    )
    rendered = render_required(task_id, spec)
    return {"status": "completed", "n_observations": len(rows), **rendered}


def execute(task_id: str, spec: dict, workflow: dict) -> dict:
    if task_id.startswith("discover_"):
        return discovery(task_id)
    kind = spec.get("kind") if isinstance(spec.get("kind"), str) else workflow_task_kind(workflow, task_id)
    if task_id.startswith("validate_") and kind != "computation":
        return validation(task_id)
    if task_id == "data_acquisition":
        return data_acquisition(task_id, spec)
    if task_id == "data_import":
        # Clinical-trial scaffold uses `data_import` instead of `data_acquisition`.
        return data_acquisition(task_id, spec)
    if task_id == "qc_preprocessing":
        return qc_preprocessing(task_id, spec)
    if task_id in {"normalisation", "normalization"}:
        return normalisation(task_id, spec)
    if task_id == "differential_expression":
        return differential_expression(task_id, spec)
    if task_id == "pathway_enrichment":
        return pathway_enrichment(task_id, spec)
    if task_id == "reporting":
        return reporting(task_id, spec)
    if task_id == "final_reporting":
        return final_reporting(task_id, spec)
    if task_id == "clinical_endpoint_analysis":
        return clinical_endpoint_analysis(task_id, spec)
    if task_id == "clinical_safety_summary":
        return clinical_safety_summary(task_id, spec)
    if task_id == "clinical_subgroup_analysis":
        return clinical_subgroup_analysis(task_id, spec)
    if task_id == "clinical_sensitivity_analysis":
        return clinical_sensitivity_analysis(task_id, spec)
    # Generic fixture completion for non-specialized computation nodes in
    # small DAGs. If the task has plot affordances, _generic_completion
    # still renders and validates them.
    return _generic_completion(task_id, spec, "generic deterministic fixture completion")


def main() -> int:
    workflow = load_json(WORKFLOW, {}) or {}
    task_id = choose_task(workflow)
    if not task_id:
        print("[fixture-plots] no ready/running task")
        return 0
    out = OUTPUTS / task_id
    out.mkdir(parents=True, exist_ok=True)
    write_container_proof(task_id)
    from_status = task_status(workflow, task_id)
    spec = task_spec(task_id)
    append(out / "progress.log", f"[{now()}] fixture-plots: starting {task_id}")
    append(out / "progress.log", f"[{now()}] fixture-plots: reading task spec and dependencies")
    try:
        result = execute(task_id, spec, workflow)
        append(out / "progress.log", f"[{now()}] fixture-plots: writing completed state patch")
        complete(task_id, from_status, result)
    except Exception as exc:
        append(out / "progress.log", f"[{now()}] fixture-plots: blocking: {exc}")
        block(task_id, from_status, str(exc))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
PY
