# Scenario 12 — Mock SARIMA monthly hospital-admission forecast

**Purpose:** Drive the `time_series_forecast` plan-class plugin
end-to-end: intake prose → `ProjectClass::TimeSeries` classification
→ `time-series-forecast.yaml` taxonomy loaded → DAG emitted →
package routed through the time-series container.

Closes the `testdata/scenarios/12-time-series-forecast/` follow-up
the conversation-fixture corpus README has flagged since the wave-8
fixtures landed (45 / 49 / 50). The scenario sits next to the bio
scenarios so the `make ivd-cross-version`-style harness can exercise
the same compile / emit / verify loop on a non-bio class without
forcing the bio classifier path.

**Synthetic data.** `monthly_admissions.csv` ships a 96-month series
(2017-01 → 2024-12) of mock hospital admissions per ICD-10 chapter,
generated from a deterministic `numpy.random.default_rng(42)` draw
with a baseline level + seasonal sinusoid + AR(1) residual + a few
hand-placed anomalies. No real PHI; no traceable cohort.

**Prompt shape.** See `request.md`. The SME prose mentions
"forecast horizon", "SARIMA", "ADF stationarity", "backtest holdout",
"MAPE", "RMSE", and "monthly hospital admissions" — enough keywords
that the classifier routes to `TimeSeries` on the first prose turn.
The clinical-trial vocabulary ("frozen SAP", "primary endpoint")
intentionally appears nowhere; the routing test is binary.

**Expected behaviour.**
1. `classify_project_class` returns `TimeSeries`.
2. `taxonomy_path_for_class` loads `time-series-forecast.yaml`.
3. DAG contains the time-series scaffold:
   `data_acquisition` → `metadata_harmonization` → `exploratory_analysis`
   → `forecasting_inference` → `interpretation` → `final_reporting`.
4. The `exploratory_analysis` stage's `required_figures` resolves to
   `decomposition_panel` and `acf_pacf_panel`; the
   `forecasting_inference` stage's resolves to `forecast_ribbon` and
   `anomaly_timeline` (per `lib/plotting/stages/exploratory_analysis.py`
   and `forecasting_inference.py`).
5. On confirm, emit produces `policies/container.json` with the
   time-series default image (currently `scripps/scripps-bio-base`
   under the host-mode fallback per ADR 0027).
6. The confirmatory-mode dropdown is **not** required (default is
   exploratory for the time-series class, per D8).

**Cross-references.**

- Conversation fixtures 45 / 49 / 50 exercise the chat-side tool-loop
  for the same class against `MockLlmBackend`.
- `e2e/fixtures/scenarios/12-time-series-forecast.yaml` drives the
  Playwright SSE flow against the live mock server.
- `tests/conversation-fixtures/README.md` references this directory
  as the missing piece in the cross-corpus map; once this scenario
  ships the README's "not present on disk" note can be retired.
