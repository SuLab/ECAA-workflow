# computational biology — ECAA analysis package

**What was asked:** Data-dependent acquisition (DDA) LC-MS/MS proteomics — TMT, SILAC, or
label-free MaxLFQ. Standard pipeline: acquire raw spectra, search
peptides (MaxQuant / FragPipe), quantify proteins, test for differential
abundance, then enrichment.


This is a self-contained, re-executable [RO-Crate](https://www.researchobject.org/ro-crate/) + [BagIt](https://www.rfc-editor.org/rfc/rfc8493) package emitted by the ECAA compiler. It bundles the analysis plan, the agent-executed code, every result table and figure, and a complete provenance trail. **You do not need to read every file** — start here.

## 1. The answer

- [`runtime/outputs/final_reporting/`](runtime/outputs/final_reporting/) — the narrative report (`report.md` / `final_report.md`) plus its figures and tables.
- [`runtime/outputs/reporting/`](runtime/outputs/reporting/) — the narrative report (`report.md` / `final_report.md`) plus its figures and tables.

## 2. The order things ran

See [`runtime/EXECUTION-ORDER.md`](runtime/EXECUTION-ORDER.md) — the 23 steps in dependency (execution) order. Each step's outputs live under `runtime/outputs/<step_id>/` (the folder name is the step id).

## 3. Where everything is

| Path | What it is |
|---|---|
| `WORKFLOW.json` | The task DAG — the machine-readable plan, with per-task EDAM input/output types and execution order. |
| `runtime/outputs/<step>/` | Per-step results: tables, `figures/`, `agent-code.json`, logs. |
| `runtime/outputs/<step>/report.md` | Human narrative for reporting steps. |
| `ro-crate-metadata.json` | RO-Crate / Workflow-Run-Crate provenance metadata — the front door for RO-Crate tooling. |
| `package.ttl` | The same provenance as an RDF graph, for machine validation (SHACL / OWL-DL). |
| `manifest-sha512.txt`, `tagmanifest-sha512.txt` | BagIt checksums — verify integrity with `bagit.py --validate .`. |
| `runtime/proofs.jsonl`, `decisions.jsonl`, `assumptions.jsonl`, `verifier-decisions.jsonl` | Provenance sidecars: typed-edge proofs, SME/agent decisions, assumptions, and the verification trace. |
| `CONTEXT.md`, `PROMPT.md`, `AGENT-EXECUTOR.md` | The brief the execution agent ran against. |
| `SNAPSHOTS.md` | Index of the literature-evidence snapshots (written after execution, when any exist). |
| `lib/`, `runtime/plotting/`, `runtime/plotting_r/` | The plotting library used to render the figures. |

## 4. Re-run it

```
ecaa-workflow-harness --package . --agent claude
```

_Generated deterministically by the ECAA compiler from the workflow plan. The per-step results and reports referenced above are produced when the package is executed._
