# Workflow Context

**Modality:** ehr_clinical_prediction
**Domain:** computational biology
**Description:** EHR clinical-prediction analysis: derive labels from raw EHR tables
(e.g., Sepsis-3 onset via the mimic-code derived table), extract
windowed vital-sign / lab features with a mandatory leakage audit,
fit one or more model families (gradient-boosted trees plus a
sequence model), then audit calibration, fairness, and external
generalization on a separate holdout cohort. The archetype models
the canonical YAIB-style EHR-prediction pipeline (van de Water 2024
ICLR); concrete model and label choices stay method-neutral.

**EDAM topic:** topic:3325
**EDAM operation:** operation:2945
**Confidence:** medium (33%)

## SME intake text

We want to recreate the evaluation in Cramer et al. 2022 PNAS, the COVID-19 Forecast Hub probabilistic-forecast paper. The dataset is the public Reich Lab Forecast Hub on GitHub (reichlab/covid19-forecast-hub), with the Johns Hopkins CSSE COVID-19 dashboard repository (CSSEGISandData/COVID-19) as the ground truth. The evaluation period is epidemiological week 17 of 2020 through EW42 of 2021, 79 weeks total. The targets are 1-, 2-, 3-, and 4-week-ahead incident deaths. Submissions cover 57 locations (50 states, DC, and 6 territories: American Samoa, Guam, Northern Mariana Islands, Puerto Rico, US Virgin Islands), of which 55 are evaluated after excluding American Samoa and Northern Mariana Islands (zero deaths in window), plus a national forecast. Each model submits 23 probability quantiles per target per location per week.

The eligibility criteria for inclusion are: each model is designated primary by its team, covers at least 25 of 51 focal locations, includes all four forecast horizons, includes the full 23-quantile set, and submitted in at least 47 of 79 weeks. After applying these criteria, 28 models qualify. We want to reproduce Table 1 of the paper -- per-model relative WIS, relative MAE, 50% PI coverage, and 95% PI coverage.

The headline finding to verify is that the COVIDhub-ensemble achieves a relative WIS of 0.61 -- 39% less probabilistic error than the COVIDhub-baseline naive model -- while no individual model consistently outperforms the ensemble across location-target-week observations. Empirical 95% prediction interval coverage for the ensemble should be 0.90.

Secondary analyses: standardized-rank density distributions across models (Fig 2), pandemic-phase stratification across the four named phases, and per-location relative-WIS heatmaps (Fig 5).

No models are being fit here -- the forecasts are pre-computed model outputs from teams. This is purely a scoring and aggregation task, with R 4.0.2 (the analysis software stack used by Reich Lab) as the reference reproducibility environment. Compute requirements are minimal -- single workstation, no GPU.

Claims: probabilistic forecast skill comparison only. No public-health policy recommendations.

