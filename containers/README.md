# Containers

scripps-workflow's container plumbing follows a **layered** architecture (Stage 15). One universal analysis base — `bio-min` — extends out into per-domain images that add domain-specific tooling on top.

```
ghcr.io/scripps/bio-min:v0.1.0-eval         ← universal base (Python+R+aligners+QC)
├── ghcr.io/scripps/bio-gwas:v0.1.0         ← +PLINK, regenie, susie, coloc
├── ghcr.io/scripps/bio-clinical:v0.1.0     ← +CDISC ADaM tooling, admiral
├── ghcr.io/scripps/bio-spatial:v0.1.0      ← +spaceranger, Squidpy, monkeybread
├── ghcr.io/scripps/bio-time-series:v0.1.0  ← +prophet, sktime, neuralforecast
├── ghcr.io/scripps/bio-chip:v0.1.0         ← +MACS2/MACS3, deeptools
├── ghcr.io/scripps/bio-proteomics:v0.1.0   ← +Comet, Percolator, MaxQuant
└── ghcr.io/scripps/bio-longread:v0.1.0     ← +minimap2, IsoQuant, NanoCaller
```

## Resolution order

Per CLAUDE.md "Container plumbing" / Stage 15:

1. Per-task `container.image` (set by the composer from atom-level `preferred_container`)
2. Package-level `policies/container.json::image`
3. `SWFC_DEFAULT_CONTAINER_IMAGE` env var (operator-level default; recommended: `ghcr.io/scripps/bio-min:v0.1.0-eval`)
4. Bare-host execution (no container)

## Atom-level pinning convention

Atoms whose tooling fits inside bio-min directly leave `preferred_container` *unset* and rely on the operator-level default. Atoms that need domain-specific tooling reference the relevant per-domain image:

```yaml
# config/stage-atoms/peak_calling.yaml — needs MACS, deeptools
preferred_container:
  image: "ghcr.io/scripps/bio-chip"
  tag: "v0.1.0"
  network:
    kind: bridge
```

## Per-domain image authoring rules

Each per-domain Containerfile under `containers/<domain>/Dockerfile`:

1. **Extends `bio-min`.** First line is `FROM ghcr.io/scripps/bio-min:<pinned tag>`. Mamba is already in PATH; just `mamba install` the additional packages.
2. **Pins by version.** Every `mamba install` argument is `pkg=N.M.*` semver; reproducibility is a load-bearing project value.
3. **Records its package set.** The build emits `/etc/<domain>/RUNTIME_VERSIONS.json` with the additions on top of bio-min's baseline (the bio-min digest is also captured for cross-version comparison).
4. **Uses bio-min's `bio:bio` user.** No `USER root` unless an apt-install is genuinely required.
5. **Stays under 12 GB.** A per-domain image that crosses 12 GB belongs to two domains; split it.

## Phase 1 inventory

Phase 1 evaluation only requires `bio-min`. The 6 evaluation atoms (`compbio_query`, `bio_mystery_query`, `biomni_eval1_query`, `lab_bench_query`, `hle_bio_query`, `sciagent_solution`) are all "answer-the-question" atoms whose tooling needs are covered by bio-min's Python+R+aligners+QC baseline.

Per-domain images ship as their atoms ship. Until then, the Dockerfiles in `containers/bio-*` are reference implementations.

## Build + push

```bash
# Universal base (Phase 1 needs only this)
scripts/build-bio-min.sh ghcr.io/scripps/bio-min:v0.1.0-eval

# Per-domain image (representative example)
scripts/build-bio-domain.sh bio-gwas v0.1.0
```

The build scripts resolve the resulting digest and bump `crates/eval-adapters/versions.lock` so the eval-pin-verify gate stays green.
