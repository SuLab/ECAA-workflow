# MIMIC-IV Sepsis Early-Warning Prediction Package Request

Use the following local files as the source materials for this package request:

- Project overview: testdata/scenarios/08-ehr-sepsis-mimic/overview.md
- Dataset inventory (TSV): testdata/scenarios/08-ehr-sepsis-mimic/studies.tsv

Create a fully autonomous-ready internal research package for a credentialed-access ICU EHR reanalysis of sepsis early-warning prediction on MIMIC-IV (Johnson 2023 *Sci Data*), externally validated on eICU-CRD (Pollard 2018 *Sci Data*).

Primary objective: train and externally validate a gradient-boosted-tree + recurrent-neural-network ensemble classifier for Sepsis-3 onset in adult ICU patients, with internal MIMIC-IV AUROC ≥ 0.85 and external eICU-CRD AUROC ≥ 0.75, with calibration and fairness audit, and with an explicit leakage audit against the Sepsis-3 derivation window.

Context from the source files:

- MIMIC-IV and eICU-CRD are hosted on PhysioNet under **credentialed access**. The package MUST document this access tier explicitly and MUST NOT attempt to download the data autonomously — a human-in-the-loop acknowledgment is required.
- MIMIC-IV does **not** ship hand-labeled sepsis outcomes. The canonical label derivation is the `sepsis3` table from the `mimic-code` repository. ICD-only labels are a known failure mode and must be rejected.
- Feature extraction must be windowed hourly and must not overlap the Sepsis-3 derivation window; a leakage audit is mandatory.
- The YAIB framework (van de Water 2024 ICLR) provides baseline AUROC / AUPRC / calibration numbers across MIMIC-III, MIMIC-IV, eICU, HiRID, and AUMCdb for sepsis and four other tasks, and serves as the reference benchmark.
- External generalization AUROC is expected to drop 0.05–0.15 relative to internal validation. The package must not interpret this drop as a model failure.

Data availability and scope:

- MIMIC-IV v3.1 and eICU-CRD v2.0 are both credentialed-access on PhysioNet. DUA and CITI training are required.
- No auto-download is permitted. The human operator must confirm credentialed access before the package proceeds.
- No PHI leaves the host environment; no external services are called with patient-level data.
- Treat this as a credentialed-access adult ICU EHR project. Pediatric ICU, ED-only, and non-US cohorts are out of scope for the primary analysis.
- Publication scope: internal only.
- Governance: credentialed-access research data only; no re-identification attempts; no secrets; PhysioNet DUA terms apply.

Required package behavior:

- The package must be suitable for downstream autonomous execution after generation, subject to the credentialed-access human-in-the-loop gate.
- Explicit control decisions required for: (a) credentialed-access acknowledgment gate, (b) label-derivation source (`mimic-code` `sepsis3` table is canonical; ICD-only is forbidden), (c) cohort inclusion rules (adult, ≥ 6 h ICU pre-infection, no re-admissions), (d) feature-extraction framework (MIMIC-Extract vs concepts library), (e) windowing strategy and leakage audit configuration, (f) model family (XGBoost + LSTM or TCN or Transformer), (g) calibration method (Platt, isotonic, temperature), (h) fairness subgroup audit stratification (age, sex, race, hospital-of-origin), and (i) fail-closed stop conditions when internal AUROC is below 0.85, external AUROC below 0.75, Brier score above 0.10, or the leakage audit surfaces any feature crossing the Sepsis-3 derivation window.
- Conservative claim boundaries: descriptive prediction performance, calibration, and subgroup fairness audit only. No clinical-deployment, clinical-decision-support, or bedside-triage claims. Any actionable interpretation is hypothesis-generating and must be labeled as such.
- If runtime refinement occurs (e.g. excluding hospitals with insufficient follow-up), provenance and ranking logic must be logged explicitly.
- Literature grounding: at minimum Johnson 2023, Pollard 2018, Singer 2016 (Sepsis-3), Wang 2020 (MIMIC-Extract), van de Water 2024 (YAIB), Chen & Guestrin 2016 (XGBoost), and Harutyunyan 2019 must appear in the package bibliography.

## Extracted Overview Preview

```
Project goal: Sepsis-3 early-warning prediction trained on MIMIC-IV, externally validated on eICU-CRD.
Access tier: PhysioNet credentialed; DUA + CITI training required; human-in-the-loop gate mandatory.
Label: mimic-code sepsis3 derived table (ICD-only forbidden).
Features: windowed hourly via MIMIC-Extract / concepts library; leakage audit mandatory.
Models: XGBoost + LSTM/TCN/Transformer ensemble.
Thresholds: internal AUROC >= 0.85; external AUROC >= 0.75; Brier <= 0.10.
Claim boundaries: descriptive prediction performance only; no clinical-deployment claims.
```

## Extracted Dataset Inventory Preview

```
MIMIC-IV v3.1 Johnson 2023 Sci Data BIDMC Boston ~364627 patients PhysioNet credentialed (DOI 10.13026/6mm1-ek67)
eICU-CRD v2.0 Pollard 2018 Sci Data ~208 US hospitals ~200859 patients PhysioNet credentialed
mimic-code sepsis3 derived table  MIT-LCP GitHub  canonical Sepsis-3 derivation
YAIB v1.2 van de Water 2024 ICLR  wraps MIMIC-III/IV + eICU + HiRID + AUMCdb  reference benchmark
```
