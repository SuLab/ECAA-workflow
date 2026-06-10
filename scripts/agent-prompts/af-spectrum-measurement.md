## Deterministic AF-spectrum measurement (run verbatim)

This task's surviving variant set must be measured by the pinned script. Do
NOT recompute the AF spectrum by hand and do NOT pass any threshold to the
script. Run exactly:

```
python3 lib/measure_af_spectrum.py \
  --vcf <path-to-your-post-filter-VCF> \
  --out runtime/outputs/${ECAA_TASK_ID}/result.json
```

The script emits `af_values`, `variant_count`, `n_samples`, `low_af_band_count`,
and `sub_noise_floor_count` into result.json. Your goal is to preserve the
low-allele-frequency heteroplasmic tail; the harness verifies the measured
spectrum against operator-authored reference bounds you are never given.
