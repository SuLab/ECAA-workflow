I need a time-series forecast of monthly hospital admissions for the
next 12 months. We have eight years of monthly history (2017–2024) at
ICD-10-chapter granularity in `monthly_admissions.csv` and the
operations-planning team wants a SARIMA-class model with seasonal
period 12.

Forecast horizon is 12 months. Hold out the last 24 months for
backtesting. Report MAPE and RMSE on the holdout, plus per-period
forecast-interval coverage at 80 % and 95 %.

Run ADF stationarity diagnostics + first-difference / seasonal-
difference decomposition before model fit; if the series is
stationary at lag 12 only, log it and proceed with seasonal
differencing. Surface the decomposition + ACF/PACF panels so the
ops team can sanity-check the season component visually.

Highlight any month whose actual admissions land outside the 95 %
forecast interval as an anomaly on the timeline figure. We expect
the COVID-period admissions in early 2020 to flag; that's a known
structural break and the report should call it out as such (not a
modelling problem).

Inputs: `monthly_admissions.csv` (long format: `month,
icd10_chapter, admissions`). No PHI. The scenario data is synthetic
— see `overview.md`.

Deliverables: a one-page narrative report, the forecast ribbon for
each ICD-10 chapter, the anomaly timeline overlaid on the historical
series, and the standard time-series QC bundle (decomposition,
ACF/PACF, residual diagnostics).
