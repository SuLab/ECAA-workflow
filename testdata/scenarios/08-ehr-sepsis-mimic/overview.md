# MIMIC-IV Sepsis Early-Warning Prediction with External eICU Validation

## Project goal
Build and externally validate an early-warning classifier for Sepsis-3 onset in adult ICU patients, trained on the MIMIC-IV ICU EHR dataset (Johnson 2023 *Sci Data*) and externally validated on the eICU Collaborative Research Database (Pollard 2018 *Sci Data*). The deliverable is an AUROC / AUPRC / calibration curve evaluation with per-subgroup fairness audit and a leakage audit against the Sepsis-3 derivation window.

## Strategy
Derive the Sepsis-3 endpoint (suspected infection + SOFA increase ≥ 2) from MIMIC-IV using the official `mimic-code` `sepsis3` derived table, then assemble a prediction-window cohort of adult ICU admissions with at least 6 hours of ICU time prior to the first suspected infection. Construct hourly feature tables using MIMIC-Extract or the MIMIC-IV concepts library, and train both gradient-boosted trees (XGBoost) and a recurrent / transformer baseline (LSTM or TCN) to predict Sepsis-3 onset in a forward-looking window. Externally validate on eICU-CRD with the same feature extraction logic and report AUROC, AUPRC, and calibration, stratified by age, sex, race, and hospital of origin. Audit for data leakage by verifying that no feature observed at or after the Sepsis-3 derivation window is present in the training features.

## Key challenges
- **Credentialed access:** MIMIC-IV and eICU-CRD are hosted on PhysioNet under credentialed access requiring CITI training + signed DUA. The package must document this access tier explicitly and must not attempt to download the data autonomously without human-in-the-loop acknowledgment.
- **Sepsis label derivation:** MIMIC-IV does **not** ship hand-labeled sepsis outcomes. The label must be derived — the canonical derivation is the `sepsis3` table from `mimic-code`. Deriving the label from ICD-9/ICD-10 codes alone is NOT recommended and the package should flag ICD-only derivations as a known failure mode.
- **Data leakage:** time-of-onset features (vitals, labs, microbiology cultures, antibiotic orders) are all susceptible to silent leakage if the feature-extraction window overlaps the Sepsis-3 derivation window. A leakage audit is mandatory.
- **Missingness structure:** ICU features are irregularly sampled; the package must decide between carry-forward imputation, masked attention, or time-aware interpolation, and document the choice.
- **External generalization:** eICU-CRD has different feature coverage, different hospital-of-origin distribution, and different vendor-EHR biases than MIMIC-IV. External validation AUROC is expected to drop 0.05–0.15 relative to internal MIMIC-IV validation; the package must not interpret this drop as a model failure.
- **Fairness and subgroup drift:** Sepsis detection is known to be harder in patients with chronic illness and easier in previously-healthy patients; subgroup stratification is essential to surface these disparities rather than bury them in the aggregate.

## Work completed so far
MIMIC-IV and eICU-CRD have become the two most-used public ICU EHR corpora, and sepsis early-warning is one of the most benchmarked tasks on them (YAIB — van de Water 2024 ICLR; Harutyunyan 2019 *Sci Data*; MIMIC-Extract, Wang 2020 CHIL). The YAIB framework packages MIMIC-III, MIMIC-IV, eICU, HiRID, and AUMCdb under 5 canonical tasks including sepsis; its Sepsis-3-derived labels and baseline XGBoost / LSTM / TCN / Transformer baselines provide a direct reference for a rerunnable package.

## Core methodological question
Whether a pinned XGBoost + LSTM ensemble trained on MIMIC-IV with the `mimic-code` `sepsis3` label derivation achieves an internal AUROC ≥ 0.85 on a held-out MIMIC-IV test split, and an external AUROC ≥ 0.75 on eICU-CRD, without data leakage and with calibration Brier score ≤ 0.10 on the external set. If any of these floors is violated, the package must stop and refuse to emit a model card.

## Potential analysis prompts
1. Verify credentialed-access acknowledgment and download MIMIC-IV v3.1 and eICU-CRD via PhysioNet (human-in-the-loop gate).
2. Execute the `mimic-code` `sepsis3` derivation to produce labels.
3. Build the MIMIC-IV cohort (adults, ≥ 6 h ICU time before suspected infection).
4. Feature extraction via MIMIC-Extract or the concepts library; windowed hourly features.
5. Train XGBoost baseline; report internal AUROC, AUPRC, calibration.
6. Train LSTM / TCN / Transformer baseline.
7. External validation on eICU-CRD with identical feature logic.
8. Subgroup audit (age, sex, race, hospital-of-origin) for fairness drift.
9. Leakage audit: confirm no feature observed at or after the Sepsis-3 derivation window appears in training features.
10. Compare against YAIB published baselines.

## Conservative claim boundaries
Descriptive prediction performance with explicit calibration and subgroup audit. No clinical-deployment claims. No claims that the model is safe to deploy at the bedside without prospective validation. Any actionable interpretation is hypothesis-generating.

## References
- Johnson AEW, Bulgarelli L, Shen L, et al. 2023. MIMIC-IV, a freely accessible electronic health record dataset. *Sci Data* 10:1. DOI 10.1038/s41597-022-01899-x. PMID 36596836.
- Pollard TJ, Johnson AEW, Raffa JD, et al. 2018. The eICU Collaborative Research Database: a freely available multi-center database for critical care research. *Sci Data* 5:180178. DOI 10.1038/sdata.2018.178.
- Singer M, Deutschman CS, Seymour CW, et al. 2016. The Third International Consensus Definitions for Sepsis and Septic Shock (Sepsis-3). *JAMA* 315:801–810. DOI 10.1001/jama.2016.0287.
- Wang S, McDermott MBA, Chauhan G, Ghassemi M, Hughes MC, Naumann T. 2020. MIMIC-Extract: a data extraction, preprocessing, and representation pipeline for MIMIC-III. *ACM CHIL '20*. DOI 10.1145/3368555.3384469.
- van de Water R, Schmidt HJ, Elbers P, et al. 2024. Yet Another ICU Benchmark: A Flexible Multi-Center Framework for Clinical ML. *ICLR 2024*. arXiv:2306.05109.
- Chen T, Guestrin C. 2016. XGBoost: A Scalable Tree Boosting System. *KDD '16*:785–794. DOI 10.1145/2939672.2939785.
- Harutyunyan H, Khachatrian H, Kale DC, Ver Steeg G, Galstyan A. 2019. Multitask learning and benchmarking with clinical time series data. *Sci Data* 6:96.
- `mimic-code` GitHub repository: https://github.com/MIT-LCP/mimic-code (sepsis3 derived table).
