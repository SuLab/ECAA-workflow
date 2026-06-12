# plotting_r — LEGACY for figures (Render-as-Contract)

Under **Render-as-Contract**, figure rendering is a FIXED, non-LLM step that runs
the shipped Python plotting library over the standardized figure-data-contract
tables a compute step leaves behind:

```
python3 -m runtime.plotting render \
    --stage <STAGE> --outputs runtime/outputs/<task_id> --required <comma-joined required_figures>
```

(`<STAGE>` = the task's `plot_stage_id` if non-null, else the task id; the
`required_figures` + `plot_stage_id` come from `runtime/outputs/<task_id>/task-spec.json`
under the `.spec` key. An empty/absent `required_figures` makes the render step a
no-op.)

## What this means for `plotting_r`

Because that render step is fixed and Python-only, `runtime/plotting_r` is **no
longer a figure contract-bearer**:

- It does **not** produce the contract figures. Figures land at
  `runtime/outputs/<task_id>/figures/<id>.{png,pdf}`, written exclusively by the
  Python render step above.
- It is **not** on the figure-validation path. The harness figure validators
  check the figures produced by the Python render step; nothing here is consulted
  to satisfy a `required_figures` obligation.
- The compute step's language no longer carries any figure-path incentive. A
  stage can compute in R (or Python, or anything else) and still get identical,
  language-uniform figures from the fixed render step — the choice of compute
  language is decoupled from figure rendering.

## Why it is still shipped

`plotting_r` is **still shipped** under `runtime/plotting_r/` (the package-copy
path in `copy_libs.rs` is unchanged) for **optional compute-side R convenience**:
an R compute step may use these `ggplot2` primitives + theme baseline for its own
ad-hoc inspection or scratch plots. Those are not contract figures, are not
validated, and are not part of the figure obligation. Do not rely on this module
to emit a `required_figures` artifact.

Nothing here is deleted or disabled — only its role in the figure contract is
retired. See `lib/plotting/__main__.py` for the fixed render entrypoint and
`lib/plotting_r/core.R` for the R primitives themselves.
