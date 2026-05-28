# Bioinformatics agent container — universal bio compute base.
#
# Build: docker build -t scripps/bioinformatics:1.0 containers/bioinformatics/
# Verify: docker run --rm scripps/bioinformatics:1.0 claude --version
#
# Architecture (per directive 2026-05-06): the container provisions
# ONLY what requires root at build time — system-level dev toolchain,
# language *runtimes* (R 4.3.2 from rocker, system Python 3.11, Node 20
# from NodeSource), and the Claude Code CLI. Modality-specific
# system tools (samtools, bowtie2, plink, kraken2, …) are added by
# the per-modality `runtime_baseline.system_packages.apt` block in
# `config/stage-taxonomies/<modality>.yaml`; the renderer in
# `crates/core/src/derived_image.rs` emits them into the derived
# image's apt layer at pre-flight build time. Language *packages*
# (Bioc / CRAN R libs, scanpy / scvi-tools / numpy / etc.) are
# installed by the executor agent at task time using the mounted
# per-session cache (`R_LIBS_USER` / `PYTHONUSERBASE` /
# `PIP_CACHE_DIR` exported by `scripts/agent-claude.sh`). First task
# in a session pays the install cost; every subsequent task in the
# same session re-uses the cached library tree.
#
# Multi-arch: linux/amd64 + linux/arm64 (rocker/r-ver multi-arch).

FROM rocker/r-ver:4.3.2@sha256:4e32addfc4da3e660f6e0d05ce5e43d3eceb9db58a60b9a142e0dde9a654ead1

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8

# System dev toolchain + dev headers covering Bioc / Seurat / scanpy
# compile chains, plus the language runtimes themselves (R from rocker,
# Python 3.11 from apt). This base deliberately includes the native
# parallel-runtime surface used by downstream language packages:
# OpenBLAS/LAPACK, OpenMP, TBB, OpenMPI, hwloc/numactl, Java, and common
# compression / file-format / geospatial / image headers. GPU-specific
# CUDA/cuDNN user-space libraries live in containers/bioinformatics-gpu;
# this CPU image still carries discovery helpers and is runnable with
# Docker --gpus / Apptainer --nv when a task only needs host driver
# passthrough plus framework-provided CUDA wheels.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl git jq gnupg procps less file unzip zip rsync \
        build-essential pkg-config cmake ninja-build meson gfortran clang \
        rustc cargo \
        python3.11 python3.11-dev python3.11-venv python3-pip \
        openjdk-17-jre-headless \
        parallel pigz zstd graphviz \
        texlive-latex-base texlive-latex-recommended texlive-fonts-recommended \
        texlive-xetex latexmk \
        libopenblas-dev liblapack-dev libblas-dev libomp-dev libtbb-dev \
        openmpi-bin libopenmpi-dev libhwloc-dev numactl \
        libcurl4-openssl-dev libssl-dev libxml2-dev libfontconfig1-dev \
        libharfbuzz-dev libfribidi-dev libfreetype6-dev libpng-dev \
        libtiff5-dev libjpeg-dev libhdf5-dev libglpk-dev libcairo2-dev \
        libpango1.0-dev libxt-dev libx11-dev \
        zlib1g-dev libbz2-dev liblzma-dev libzstd-dev libdeflate-dev \
        liblz4-dev libsnappy-dev libreadline-dev \
        libsqlite3-dev libgsl-dev libfftw3-dev libgmp-dev libmpfr-dev \
        libeigen3-dev libsuitesparse-dev libprotobuf-dev protobuf-compiler \
        libnetcdf-dev libpoppler-cpp-dev poppler-utils \
        libicu-dev libgit2-dev libarchive-dev libjemalloc2 libjemalloc-dev \
        libgdal-dev libgeos-dev libproj-dev libudunits2-dev \
        libgl1 libglib2.0-0 libxrender1 libxtst6 libxext6 \
        libmagick++-dev imagemagick pandoc fonts-dejavu-core \
        tesseract-ocr libtesseract-dev libleptonica-dev \
        pciutils ocl-icd-libopencl1 ocl-icd-opencl-dev clinfo \
    && if update-alternatives --query libblas.so.3-x86_64-linux-gnu >/dev/null 2>&1 \
          && [ -e /usr/lib/x86_64-linux-gnu/openblas-pthread/libblas.so.3 ]; then \
        update-alternatives --set libblas.so.3-x86_64-linux-gnu \
          /usr/lib/x86_64-linux-gnu/openblas-pthread/libblas.so.3; \
      fi \
    && if update-alternatives --query liblapack.so.3-x86_64-linux-gnu >/dev/null 2>&1 \
          && [ -e /usr/lib/x86_64-linux-gnu/openblas-pthread/liblapack.so.3 ]; then \
        update-alternatives --set liblapack.so.3-x86_64-linux-gnu \
          /usr/lib/x86_64-linux-gnu/openblas-pthread/liblapack.so.3; \
      fi \
    && ldconfig -p | grep -q libopenblas \
    && Rscript --vanilla -e 'blas <- extSoftVersion()[["BLAS"]]; stopifnot(grepl("openblas|blas", tolower(blas))); cat("R BLAS:", blas, "\n")' \
    && python3.11 -c 'import ctypes.util; missing=[lib for lib in ("openblas","gomp","tbb","mpi","hdf5","zstd") if ctypes.util.find_library(lib) is None]; assert not missing, missing; print("native scientific libraries present")' \
    && rm -rf /var/lib/apt/lists/*

ENV NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility \
    OMP_NUM_THREADS=1 \
    OPENBLAS_NUM_THREADS=1 \
    MKL_NUM_THREADS=1 \
    NUMEXPR_NUM_THREADS=1 \
    TBB_NUM_THREADS=1

# Node 20 LTS via NodeSource (rocker base ships node 12; Claude Code
# CLI requires node>=18).
# F-DEP-L-05: download-then-verify. NodeSource regenerates the script
# per fetch; the SHA below is the snapshot resolved 2026-05-14 and must
# rotate together with deliberate base-image rebuilds.
# TODO(phase-5-2026-05-14): refresh NODESOURCE_SETUP_SHA256 on every
# base-image rebuild — pull the script, sha256sum it, diff against the
# previous pin, then commit.
ARG NODESOURCE_SETUP_SHA256=2c4c6683a17b6f4128898a7b521e3c8bb725a99ffaf1b5e32ac97c6fa7d381be
RUN curl -fsSL https://deb.nodesource.com/setup_20.x -o /tmp/ns.sh \
    && echo "${NODESOURCE_SETUP_SHA256}  /tmp/ns.sh" | sha256sum -c - \
    && bash /tmp/ns.sh \
    && rm /tmp/ns.sh \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# Claude Code CLI is the in-container executor. Auth flows via the
# subscription-credentials bind mount that `agent-claude.sh` configures
# (`-v $HOME/.claude:$HOME/.claude:ro`).
RUN npm install -g @anthropic-ai/claude-code

# Default CRAN mirror so the agent's first `install.packages()` doesn't
# prompt for a mirror in non-interactive mode. BiocManager itself is a
# CRAN package, so the same repo bootstraps the entire bio install
# tree (BiocManager::install routes Bioc + CRAN deps).
RUN echo 'options(repos = c(CRAN = "https://cloud.r-project.org"))' \
    > /usr/local/lib/R/etc/Rprofile.site

WORKDIR /work
CMD ["claude", "--version"]
