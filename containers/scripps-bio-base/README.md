# scripps-bio-base

**Plan reference:** §S15.5 + §S15.16 (per-task SBOM).

Baseline OCI image for bioinformatics workloads. Provides the Python 3.12 scientific stack (numpy/pandas/scipy/matplotlib/scanpy/anndata/h5py/pyarrow) inside a distroless `gcr.io/distroless/cc-debian12:nonroot` final layer so the runtime size stays under ~500 MB.

## Build

```bash
podman build -t ghcr.io/scripps/scripps-bio-base:0.1.0 \
  -f containers/scripps-bio-base/Containerfile \
  containers/scripps-bio-base/
```

For reproducibility (per plan §S5.20 + ADR 0024):

```bash
SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) \
  podman build --timestamp 0 \
  -t ghcr.io/scripps/scripps-bio-base:0.1.0 \
  -f containers/scripps-bio-base/Containerfile \
  containers/scripps-bio-base/
```

CI builds via the pending `.github/workflows/container-build.yml` workflow (S15.5 follow-up); GHCR push is gated on cosign keyless signing per ADR 0026.

## Cadence

D-R15 — quarterly release. Last 2 minors supported. v0.1 baseline pinned to:

- Apptainer 1.4.x (consumer-side; image is OCI so Apptainer pulls + caches it natively via `--nv` / `--bind`)
- R 4.4.x (separate image — `containers/bioinformatics/Dockerfile` carries R; this image is Python-only by design)
- Python 3.12.x

Roll-forward driven by S5.7 bioinformatics tooling refresh sweeps; each release rolls one minor on each pin where the test corpus is green.

## SBOM + signing

Per ADR 0026 + §S15.16:

- Build emits a CycloneDX SBOM via Syft: `syft ghcr.io/scripps/scripps-bio-base:0.1.0 -o cyclonedx-json > sbom.cdx.json`
- Cosign keyless signing via GitHub Actions OIDC: `cosign sign --yes ghcr.io/scripps/scripps-bio-base@<digest>`
- Per-task SBOM emission (S15.16) layers Syft on top of the resolved digest at emit time; the result lands as a CreativeWork entity in the emitted package's RO-Crate.

## Network policy

Default: `bridge` (inherit harness default). For `clinical_trial` archetype builds (per ADR 0028 / D-R14), the container is run with `--network=none` and an allowlist applied at the harness layer.

## GPU

Not GPU-baked. Add `--nv` (Apptainer) or `--gpus=all` (Docker) at run time for stages whose `resource_profile.gpu` is true.
