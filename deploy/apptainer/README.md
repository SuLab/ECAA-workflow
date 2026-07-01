# HPC deployment (Apptainer)

HPC clusters forbid the Docker daemon, so the ORCHESTRATOR is NOT containerized here — run
`ecaa-workflow-server`/`-harness` on the login node (native binary from a GitHub release, or an
Apptainer instance). Tasks run daemonless via Apptainer:

1. `deploy/apptainer/build-sif.sh ghcr.io/scripps/bio-min:<tag> bio-min.sif`
   Air-gapped: build on a connected host with `apptainer build bio-min.sif docker-daemon://bio-min:local`, then `scp` the SIF.
2. Point the SLURM executor at the SIF and set `ECAA_SLURM_CONTAINER_RUNTIME=apptainer`
   (agent-claude.sh already branches on this, adding `--nv`/`--rocm` for GPUs).
3. SLURM-native `--container`/pyxis are admin-gated; Apptainer is the portable baseline.
   `ECAA_SLURM_NATIVE_CONTAINER=1` is an opt-in optimization on SLURM 25.11+.
