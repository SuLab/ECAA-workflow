# rocm-agent — AMD/ROCm-enabled sibling of the bio-min agent image.
#
# Why this exists: passing /dev/kfd + /dev/dri into the container
# (scripts/agent-claude.sh, AMD branch) only exposes the *devices*. The
# ROCm *userspace* (libhsa-runtime64, libamdhip64, rocblas, the HSA
# loader) must already be inside the image or GPU-backed libraries fail
# to load at runtime. ghcr.io/scripps/bio-min ships no ROCm userspace,
# so this image layers the agent runtime + the scientific Python stack
# onto an official ROCm base that already carries that userspace.
#
# Target hardware: AMD Radeon RX 6800 XT (gfx1030, RDNA2), ROCm 7.x.
# gfx1030 is not in the default rocblas/Tensile ISA tune set for some
# library builds; the host-side AMD branch exports
# HSA_OVERRIDE_GFX_VERSION=10.3.0 (ECAA_ROCM_GFX_OVERRIDE) so those libs
# target the gfx1030 ISA. Nothing needs baking into the image for that.
#
# Build (operator — multi-GB, do NOT build in CI by default):
#   docker build -t ghcr.io/scripps/bio-min-rocm:v0.1.0 \
#     -f docker/rocm-agent.Dockerfile docker/
#
# Run (the harness does this automatically once the AMD branch fires):
#   docker run --rm \
#     --device=/dev/kfd --device=/dev/dri \
#     --group-add "$(getent group video | cut -d: -f3)" \
#     --group-add "$(getent group render | cut -d: -f3)" \
#     --security-opt seccomp=unconfined \
#     -e HSA_OVERRIDE_GFX_VERSION=10.3.0 \
#     ghcr.io/scripps/bio-min-rocm:v0.1.0 rocminfo | grep -i gfx1030
#
# Point the harness at it for AMD hosts via the package container image
# or ECAA_DEFAULT_CONTAINER_IMAGE; ECAA_CONTAINER_GPU_VENDOR=auto then
# resolves to amd on a host with /dev/kfd.

# Official ROCm dev base: ships the full ROCm userspace + HIP toolchain
# on Ubuntu 22.04. Pin by digest on a deliberate bump (run
# `docker buildx imagetools inspect rocm/dev-ubuntu-22.04:6.2`). The tag
# is left version-pinned rather than digest-pinned here because the
# operator selects the ROCm minor that matches the host driver.
ARG ROCM_IMAGE=rocm/dev-ubuntu-22.04:6.2
FROM ${ROCM_IMAGE} AS rocm-agent

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    PATH=/opt/conda/bin:/opt/rocm/bin:$PATH \
    OMP_NUM_THREADS=1 \
    OPENBLAS_NUM_THREADS=1 \
    MKL_NUM_THREADS=1 \
    NUMEXPR_NUM_THREADS=1 \
    TBB_NUM_THREADS=1 \
    # gfx1030 (RX 6800 XT) ISA override. The host AMD branch also sets
    # this; baking a default in keeps a direct `docker run` of this image
    # working without the operator remembering the flag. Override or
    # clear at run time as needed.
    HSA_OVERRIDE_GFX_VERSION=10.3.0

# System surface: build toolchain + the native libraries the scientific
# Python/R stack links against, mirroring bio-min's apt layer. The ROCm
# userspace is already present from the base image, so we do NOT
# reinstall it here.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        ca-certificates curl git gnupg procps less file unzip zip rsync jq \
        build-essential pkg-config cmake ninja-build meson gfortran clang \
        pciutils \
        libcurl4-openssl-dev libssl-dev libxml2-dev \
        zlib1g-dev libbz2-dev liblzma-dev libzstd-dev libdeflate-dev \
        libhdf5-dev libsqlite3-dev \
 && rm -rf /var/lib/apt/lists/*

# Mambaforge for fast conda solves, matching bio-min. Pinned for
# reproducibility; bump deliberately and refresh the sha256.
ARG MAMBAFORGE_VERSION=24.3.0-0
ARG MAMBAFORGE_SHA256=0be3654cc3b9c43d3aeeeca5efe6d2f31e9f7711702f3818529b367b3db677fb
RUN curl -fsSL -o /tmp/mambaforge.sh \
        "https://github.com/conda-forge/miniforge/releases/download/${MAMBAFORGE_VERSION}/Mambaforge-${MAMBAFORGE_VERSION}-Linux-x86_64.sh" \
 && echo "${MAMBAFORGE_SHA256}  /tmp/mambaforge.sh" | sha256sum -c - \
 && bash /tmp/mambaforge.sh -b -p /opt/conda \
 && rm /tmp/mambaforge.sh \
 && /opt/conda/bin/conda config --system --set always_yes true \
 && /opt/conda/bin/conda config --system --set channel_priority flexible \
 && /opt/conda/bin/conda config --system --add channels bioconda \
 && /opt/conda/bin/conda config --system --add channels conda-forge \
 && /opt/conda/bin/conda clean --all -f -y

# Scientific Python baseline (CPU build; GPU acceleration comes from
# ROCm-aware wheels the agent installs at runtime, e.g.
# `pip install torch --index-url https://download.pytorch.org/whl/rocm6.2`).
RUN mamba install -n base -y \
        python=3.11 \
        "numpy>=1.26" \
        "pandas>=2.0" \
        "scipy>=1.11" \
        "scikit-learn>=1.4" \
        "matplotlib-base>=3.7" \
        "pyyaml>=6" \
        "requests>=2.30" \
        "h5py>=3.10" \
        pip \
 && mamba clean --all -f -y

# Agent runtime: Node.js + Claude Code, matching bio-min. The agent
# script is mounted at run time by the harness.
ARG NODE_VERSION=20
RUN curl -fsSL "https://deb.nodesource.com/setup_${NODE_VERSION}.x" -o /tmp/ns.sh \
 && bash /tmp/ns.sh \
 && rm /tmp/ns.sh \
 && apt-get update \
 && apt-get install -y --no-install-recommends nodejs \
 && rm -rf /var/lib/apt/lists/* \
 && npm install -g @anthropic-ai/claude-code

# Smoke the ROCm userspace at build time (binaries present) so a broken
# base fails the build. We do NOT run `rocminfo` here: `docker build`
# binds no GPU devices, so it would find no agent and fail the build on
# every host. Actual gfx1030 enumeration is a run-time check (see the
# header recipe) on the first GPU task, where /dev/kfd is bound in.
RUN command -v rocminfo >/dev/null && command -v rocm-smi >/dev/null \
 && echo "rocm userspace present"

# Work-dir conventions match bio-min (harness binds inputs/scratch/outputs).
WORKDIR /work
RUN mkdir -p /work/inputs /work/scratch /work/outputs

# Non-root agent user. The host AMD branch adds the video/render GIDs as
# supplementary groups at run time, which is what actually grants this
# user access to /dev/kfd and /dev/dri (proven on the host).
RUN useradd --create-home --uid 1001 bio
USER bio:bio

# NO ENTRYPOINT — same contract as bio-min: the harness passes the full
# `claude …` command as the docker CMD.
