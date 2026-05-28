# Scenario 11 — Clinical-trial Mock Phase III

**Purpose:** Drive the Wave 8.E clinical-trial plugin end-to-end: intake prose →
`ProjectClass::ClinicalTrial` classification → `clinical-trial-analysis.yaml`
taxonomy loaded → DAG emitted → package routed through the clinical-trial
container.

**Synthetic data.** All files under `adsl.csv`, `adae.csv`, `adlb.csv` are
mock — randomly drawn values matching the SDTM/ADaM column shape the
`cdisc_mapping` stage consumes. No patient identity, no real trial data.

**Prompt shape.** See `request.md`. The SME prose mentions "Phase III",
"randomized controlled trial", "frozen SAP", "ITT population", "primary
endpoint", "hazard ratio" — enough keywords that the classifier routes
to `ClinicalTrial` on the first prose turn.

**Expected behaviour.**
1. `classify_project_class` returns `ClinicalTrial`.
2. `taxonomy_path_for_class` loads `clinical-trial-analysis.yaml`.
3. DAG contains 9 stages: `cdisc_mapping` → ... → `reporting`.
4. On confirm, emit produces `policies/container.json` with
   `{"image": "scripps/clinical-trial:1.0"}`.
5. The confirmatory-mode dropdown is required (no default for ClinicalTrial
   class; see plan §8.C.2 / D8).
