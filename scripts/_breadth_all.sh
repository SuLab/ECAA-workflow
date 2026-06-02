#!/usr/bin/env bash
# Self-contained token-free breadth driver.
# Compiles each modality intake, audits the DAG, registers the scenario's real
# mini-data, executes via the containerized (bio-min) fixture agent, and verifies
# every plot-eligible task completed with its required figures. Zero agent tokens.
#   scripts/_breadth_all.sh            -> all scenarios
#   scripts/_breadth_all.sh NAME       -> one scenario
set -uo pipefail
cd /home/a/scripps/ecaa-workflow
source scripts/_campaign-env.sh
CLI=./target/release/ecaa-workflow
HARNESS=./target/release/ecaa-workflow-harness
DATAROOT=testdata/scenarios/atoms
OUT=/tmp/breadth
RES=/tmp/breadth_results.txt
mkdir -p "$OUT"

INTAKES_JSON="$OUT/_intakes.json"
cat > "$INTAKES_JSON" <<'JSON'
{
 "scrna-pbmc1k": "I have a 10x Genomics single-cell RNA-seq dataset of about 1000 human PBMCs. Please run the standard scRNA-seq workflow: QC and filtering of low-quality cells, normalization, dimensionality reduction, clustering, and cell-type annotation, with UMAP and QC diagnostic plots and a final report.",
 "scatac-encode-chr22": "I have a single-cell ATAC-seq dataset restricted to chromosome 22 from ENCODE. Please run the standard scATAC analysis: QC, peak calling, dimensionality reduction and clustering of cells by accessibility, peak annotation, and differential accessibility between clusters, with diagnostic plots and a final report.",
 "cuttag-encode-chr22": "I have CUT&Tag data for a histone mark on chromosome 22 from ENCODE. Please run the standard CUT&Tag workflow: QC, peak calling, peak annotation, and motif enrichment over the called peaks, with coverage and peak diagnostic plots and a final report.",
 "gwas-1kg-chr22": "I have GWAS summary statistics on chromosome 22 plus a 1000 Genomes LD reference. Please run a standard GWAS post-processing and colocalization analysis: harmonize the summary statistics, run colocalization against an eQTL panel, and report credible sets, with Manhattan/Miami and colocalization plots and a final report.",
 "wgs-giab-chr22": "I have whole-genome sequencing reads for the GIAB NA12878 sample restricted to chromosome 22. Please run a standard germline variant-calling benchmark: alignment, variant calling, variant filtering, and variant annotation, with diagnostic plots and a final report.",
 "methylation-roadmap-chr22": "I have Illumina methylation array data on chromosome 22 from Roadmap. Please run a standard methylation workflow: QC and normalization of beta values, differential methylation analysis between groups, and correlation of methylation with expression, with diagnostic plots and a final report.",
 "metagenomics-16s-mini": "I have 16S rRNA amplicon sequencing data from a small set of gut microbiome samples. Please run the standard microbiome workflow: QC, taxonomic classification, and diversity analysis comparing cases and controls, with abundance and diversity plots and a final report.",
 "proteomics-massspec-mini": "I have a small label-free mass-spectrometry proteomics dataset comparing two conditions. Please run the standard proteomics workflow: QC, peptide and protein identification and quantification, and differential abundance analysis between conditions, with diagnostic plots and a final report.",
 "spatial-dlpfc-mini": "I have a small 10x Visium spatial transcriptomics dataset of human DLPFC tissue. Please run the standard spatial workflow: QC, normalization, spatial domain segmentation, and detection of spatially variable genes, with spatial plots and a final report.",
 "cross-omics-rna-atac": "I have paired bulk RNA-seq and ATAC-seq from the same samples and want a multi-omics integration. Please run RNA-seq differential expression, ATAC-seq peak calling and differential accessibility, link peaks to genes, and integrate the two modalities, with diagnostic plots and a final report.",
 "clinical-trial-mock": "I have a mock phase-3 randomized clinical trial dataset comparing a treatment arm to placebo. Please run a standard clinical-trial analysis: define the analysis population, extract clinical features, run the primary endpoint analysis, and a subgroup/sensitivity analysis, with a CONSORT-style flow and forest/survival plots and a final report.",
 "time-series-admissions": "I have a daily time series of hospital admissions over a couple of years. Please run a standard time-series workflow: exploratory decomposition, feature engineering, model fitting and forecasting, with forecast and decomposition plots and a final report."
}
JSON

run_scenario() {
    local name pkg prose pf audit nt a data
    name="$1"
    pkg="$OUT/$name"
    data="$DATAROOT/$name"
    prose=$(jq -r --arg k "$name" '.[$k]' "$INTAKES_JSON")
    pf="$OUT/$name.intake.txt"
    printf '%s\n' "$prose" > "$pf"
    rm -rf "$pkg"
    if ! "$CLI" intake --input "$pf" --output "$pkg" --config config >"$OUT/$name.emit.log" 2>&1; then
        echo "EMIT-FAIL $name :: $(tail -1 "$OUT/$name.emit.log")" >> "$RES"
        return
    fi
    a=$(python3 scripts/audit_dag.py "$pkg/WORKFLOW.json" 2>&1)
    nt=$(echo "$a" | grep -oE 'tasks: [0-9]+' | head -1 | grep -oE '[0-9]+')
    audit="AUDIT-PASS"
    echo "$a" | grep -q 'RESULT: PASS' || audit="AUDIT-FAIL"
    # register the scenario's real mini-data as the workflow input
    if [ -d "$data" ]; then
        ECAA_RS_PKG="$pkg" ECAA_RS_DATA="$data" ECAA_RS_NAME="$name" python3 - <<'PY'
import json, os
pkg = os.environ["ECAA_RS_PKG"]; data = os.environ["ECAA_RS_DATA"]; name = os.environ["ECAA_RS_NAME"]
files = [{"relpath": f, "size_bytes": os.path.getsize(os.path.join(data, f))}
         for f in sorted(os.listdir(data))
         if os.path.isfile(os.path.join(data, f)) and f != "SHA256SUMS"]
inp = [{"input_id": "in_fixture", "label": name, "kind": "local_path",
        "root_path": os.path.abspath(data), "files": files}]
os.makedirs(os.path.join(pkg, "runtime"), exist_ok=True)
json.dump(inp, open(os.path.join(pkg, "runtime", "inputs.json"), "w"), indent=2)
PY
    fi
    "$HARNESS" --package "$pkg" --agent scripts/agent-fixture-plots.sh --max-iterations 250 >"$pkg/harness.log" 2>&1
    if [ -d "$pkg" ]; then
        git -C "$pkg" init >/dev/null 2>&1 || true
        git -C "$pkg" config user.name "Scripps Breadth QA" >/dev/null 2>&1 || true
        git -C "$pkg" config user.email "breadth-qa@scripps.local" >/dev/null 2>&1 || true
        git -C "$pkg" add -A >/dev/null 2>&1 || true
        if ! git -C "$pkg" diff --cached --quiet >/dev/null 2>&1; then
            git -C "$pkg" commit -m "test: commit breadth harness artifacts" >/dev/null 2>&1 || true
        fi
    fi
    ECAA_RS_PKG="$pkg" ECAA_RS_NAME="$name" ECAA_RS_NT="$nt" ECAA_RS_AUDIT="$audit" ECAA_RS_RES="$RES" python3 - <<'PY'
import json, os, subprocess
pkg = os.environ["ECAA_RS_PKG"]; name = os.environ["ECAA_RS_NAME"]
nt = os.environ["ECAA_RS_NT"]; audit = os.environ["ECAA_RS_AUDIT"]; res = os.environ["ECAA_RS_RES"]
wf = json.load(open(os.path.join(pkg, "WORKFLOW.json"))); T = wf["tasks"]
def st(t):
    s = t.get("state", {})
    return s.get("status") if isinstance(s, dict) else s
inc = [k for k, t in T.items() if st(t) != "completed"]
miss = []; figs = 0; cproof = 0
for k, t in T.items():
    spec = t.get("spec") or {}
    fd = os.path.join(pkg, "runtime", "outputs", k, "figures")
    have = set(os.listdir(fd)) if os.path.isdir(fd) else set()
    for f in (spec.get("required_figures") or []):
        if any(h == f + ".png" or h.startswith(f + ".") for h in have):
            figs += 1
        else:
            miss.append(f"{k}/{f}")
    if os.path.exists(os.path.join(pkg, "runtime", "outputs", k, "container-proof.json")):
        cproof += 1
rev = subprocess.run(["git", "-C", pkg, "rev-parse", "--is-inside-work-tree"], capture_output=True, text=True)
status = subprocess.run(["git", "-C", pkg, "status", "--porcelain"], capture_output=True, text=True)
git_ok = rev.returncode == 0 and rev.stdout.strip() == "true" and status.returncode == 0 and status.stdout.strip() == ""
ok = (audit == "AUDIT-PASS") and not inc and not miss and git_ok
tag = "PASS" if ok else "FAIL"
line = f"{tag} {name} tasks={nt} {audit} figs={figs} cproof={cproof}"
if inc:
    line += f" INCOMPLETE={inc}"
if miss:
    line += f" MISSFIG={miss[:5]}"
if not git_ok:
    line += " GIT-FAIL"
open(res, "a").write(line + "\n")
print(line)
PY
}

if [ "$#" -ge 1 ]; then
    run_scenario "$1"
else
    : > "$RES"
    for n in $(jq -r 'keys[]' "$INTAKES_JSON"); do
        run_scenario "$n"
    done
    echo "ALL_DONE" >> "$RES"
fi
