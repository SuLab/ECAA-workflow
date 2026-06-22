## Deterministic effect-size-reliability measurement (run verbatim)

This task's differential-expression results must be measured by the pinned
script. Do NOT recompute the metric by hand and do NOT pass any threshold to
the script. Point `--table` at THIS task's differential-expression results
table (the table carrying a per-feature effect-size column and, when your
analysis produced one, a per-feature abundance/information column). Run exactly:

```
python3 lib/measure_de_effect_size.py \
  --table <path-to-this-task's-de_results.tsv> \
  --out runtime/outputs/${ECAA_TASK_ID}/result.json
```

The script emits `information_column_recorded` and
`top_effect_abundance_ratio` into result.json (plus informational
`top_effect_k` and `tested_feature_count`). It ALSO emits two
report-completeness presence flags — `r_squared_column_recorded` and
`sample_size_column_recorded` — set true only when your OWN results table
records a model-fit / variance-explained column or a per-row sample-size column
(found AND with at least one usable value). When such a column is absent the
flag is false and the corresponding report-completeness check is skipped — it
never requires you to add a column. Preserve any fields you also write
(design_formula, response_variable, available_covariates, status, method,
narrative, claims) by MERGING them with the script's output rather than
overwriting it.

The script folds your written answer into a single `narrative_text` field of
result.json so the harness can confirm you SURFACED the statistics your own
table records. It reads your narrative deliverable from the SAME directory as
`--out` (`runtime/outputs/${ECAA_TASK_ID}/`): write your answer there as a
report.md / interpretation.md / summary.md / answer.txt (and/or a `narrative`
string in result.json) BEFORE you run the script, so it is folded in. When your
table records a model-fit statistic (an R-squared) or a per-row sample size,
state that statistic in your answer in your own words — HOW you phrase it is
your choice; no wording, method, or value is prescribed.

The script recomputes, from your OWN results table, a single scale-free ratio:
the typical (median) abundance/information of the features you ranked as
strongest by effect size, divided by the typical abundance/information across
your whole tested set — a domain-correctness fact about your own output. A ratio
near 1 means your strongest-by-effect features are as well-supported as a random
draw; a ratio near 0 means your strongest findings sit at a small fraction of
the typical support, where effect estimates are unreliable. The harness verifies
that recomputed value against an operator-authored reference bound you are never
given; how you keep your strongest-finding list domain-correct is your choice (no
method, tool, or threshold value is prescribed). When your table carries no
abundance/information column the script records `information_column_recorded:
false` and the check is skipped — it never requires you to add such a column.
