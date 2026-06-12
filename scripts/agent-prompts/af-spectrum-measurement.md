## Deterministic AF-spectrum measurement (run verbatim)

This task's variant set must be measured by the pinned script. Do NOT recompute
the AF spectrum by hand and do NOT pass any threshold to the script. Point
`--vcf` at THIS task's output VCF (the called VCF for variant calling, the
filtered VCF for variant filtering); if you produced per-sample VCFs, merge them
into one multi-sample VCF first so the sample count is recorded. Run exactly:

```
python3 lib/measure_af_spectrum.py \
  --vcf <path-to-this-task's-output-VCF> \
  --out runtime/outputs/${ECAA_TASK_ID}/result.json
```

The script emits `af_values`, `variant_count`, `n_samples`,
`variant_count_per_sample`, `min_surviving_af`, `low_af_band_count`, and
`sub_noise_floor_count` into result.json. Preserve any fields you also write
(status, method, narrative, claims) by merging them with the script's output
rather than overwriting it. Your goal is to preserve the low-allele-frequency
heteroplasmic tail; the harness verifies the measured spectrum against
operator-authored reference bounds you are never given.
