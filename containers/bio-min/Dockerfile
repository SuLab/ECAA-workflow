# bio-min — minimal bioinformatics base image used by the Phase 1
# evaluation harness. Per evaluation-phase-1-plan.md §6.3 + spec
# §4.1 atom-catalog table. Wraps the agent execution surface;
# `scripts/agent-claude.sh` runs Claude Code inside this image so the
# tools the agent might invoke at runtime resolve under the same
# pinned binary set across every benchmark run.
#
# Build:
#   docker build -t ghcr.io/scripps/bio-min:v0.1.0-eval \
#                -f containers/bio-min/Dockerfile containers/bio-min/
#
# Pin (after build):
#   docker manifest inspect ghcr.io/scripps/bio-min:v0.1.0-eval \
#     | jq -r '.config.digest' \
#     | xargs -I {} sed -i \
#         "s|sha256:0000000000000000000000000000000000000000000000000000000000000000|{}|" \
#         crates/eval-adapters/versions.lock
#
# Push (operator):
#   docker push ghcr.io/scripps/bio-min:v0.1.0-eval

# ─────────────────────────────────────────────────────────────────
# Stage 1: build a layer that has the conda-forge + bioconda mirror
# baked in. We use mambaforge to keep solve times tractable; the base
# image is Debian 12 slim for a small footprint.
# ─────────────────────────────────────────────────────────────────
FROM debian:12-slim@sha256:67b30a61dc87758f0caf819646104f29ecbda97d920aaf5edc834128ac8493d3 AS conda-base

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    PATH=/opt/conda/bin:$PATH \
    NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility \
    OMP_NUM_THREADS=1 \
    OPENBLAS_NUM_THREADS=1 \
    MKL_NUM_THREADS=1 \
    NUMEXPR_NUM_THREADS=1 \
    TBB_NUM_THREADS=1

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        ca-certificates curl git gnupg procps less file unzip zip rsync \
        build-essential pkg-config cmake ninja-build meson gfortran clang \
        rustc cargo \
        openjdk-17-jre-headless \
        parallel pigz zstd graphviz \
        texlive-latex-base texlive-latex-recommended texlive-fonts-recommended \
        texlive-xetex latexmk \
        libopenblas-dev liblapack-dev libblas-dev libomp-dev libtbb-dev \
        openmpi-bin libopenmpi-dev libhwloc-dev numactl \
        libcurl4-openssl-dev libssl-dev libxml2-dev libfontconfig1-dev \
        libharfbuzz-dev libfribidi-dev libfreetype6-dev libpng-dev \
        libtiff-dev libjpeg-dev libhdf5-dev libglpk-dev libcairo2-dev \
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
 && rm -rf /var/lib/apt/lists/*

# Mambaforge for fast conda solves (libmamba). Pinned to a specific
# release for reproducibility; bump deliberately.
ARG MAMBAFORGE_VERSION=24.3.0-0
# Resolved 2026-05-14 against Mambaforge-24.3.0-0-Linux-x86_64.sh upstream
# checksum. F-DEP-L-05: pin the post-download hash so a compromised CDN
# can't substitute a tampered installer.
ARG MAMBAFORGE_SHA256=0be3654cc3b9c43d3aeeeca5efe6d2f31e9f7711702f3818529b367b3db677fb
RUN curl -fsSL -o /tmp/mambaforge.sh \
        "https://github.com/conda-forge/miniforge/releases/download/${MAMBAFORGE_VERSION}/Mambaforge-${MAMBAFORGE_VERSION}-Linux-x86_64.sh" \
 && echo "${MAMBAFORGE_SHA256}  /tmp/mambaforge.sh" | sha256sum -c - \
 && bash /tmp/mambaforge.sh -b -p /opt/conda \
 && rm /tmp/mambaforge.sh \
 && /opt/conda/bin/conda config --system --set always_yes true \
 # Was `channel_priority strict` — switched to `flexible` because
 # `strict` refused to mix conda-forge + bioconda packages, producing
 # libdeflate/libtiff/pillow-cascade UnsatisfiableErrors when
 # matplotlib-base + pyarrow + biopython are requested together. With
 # `flexible` the solver picks the best compatible version across
 # channels, which is what we want for the scientific-Python + R +
 # bioconda toolchain.
 && /opt/conda/bin/conda config --system --set channel_priority flexible \
 && /opt/conda/bin/conda config --system --add channels bioconda \
 && /opt/conda/bin/conda config --system --add channels conda-forge \
 && /opt/conda/bin/conda clean --all -f -y

# ─────────────────────────────────────────────────────────────────
# Stage 2: bioinformatics tooling — aligners, variant callers,
# QC/quantification, plus the Python + R analysis stacks.
# Per the plan's "minimal bioinformatics base" — anything specific
# that an atom needs (e.g. a custom reference genome) is added by a
# downstream image, NOT here.
# ─────────────────────────────────────────────────────────────────
FROM conda-base AS bio-min

# Python stack — standard scientific computing baseline + OpenBLAS-
# linked numpy/scipy + multiprocessing libraries (OpenMP, TBB, MPI).
# Version pins use `>=` floors rather than `=N.N.*` so the conda
# solver has slack to find a mutually-compatible set as upstream
# moves. Agents install analysis-specific packages (scanpy, anndata,
# seurat, leidenalg, pydeseq2, gseapy, etc.) at runtime via
# `pip install --break-system-packages` so atom YAMLs don't need to
# pre-declare them — that decision belongs to the agent + SME.
RUN mamba install -n base -y \
        python=3.11 \
        "libblas=*=*openblas" \
        "liblapack=*=*openblas" \
        openblas \
        llvm-openmp \
        tbb \
        openmpi \
        "numpy>=1.26" \
        "pandas>=2.0" \
        "scipy>=1.11" \
        "scikit-learn>=1.4" \
        "statsmodels>=0.14" \
        "matplotlib-base>=3.7" \
        "seaborn>=0.13" \
        "pyarrow>=14" \
        "pyyaml>=6" \
        "requests>=2.30" \
        "biopython>=1.80" \
        "anndata>=0.10" \
        "h5py>=3.10" \
        pip \
 && mamba clean --all -f -y

# Aligners + quantifiers + variant callers + QC. Same loosened-floor
# strategy as the Python stack — the solver picks compatible
# revisions from bioconda's current state without hand-tuning every
# pin on each rebuild.
RUN mamba install -n base -y \
        "samtools>=1.20" \
        "bcftools>=1.20" \
        "htslib>=1.20" \
        "bwa>=0.7.18" \
        "bowtie2>=2.5" \
        "hisat2>=2.2" \
        "star>=2.7" \
        "salmon>=1.10" \
        "kallisto>=0.50" \
        "subread>=2.0" \
        "htseq>=2.0" \
        "bedtools>=2.31" \
        "vcftools>=0.1.16" \
        "gatk4>=4.5" \
        "fastqc>=0.12" \
        "multiqc>=1.24" \
        "trimmomatic>=0.39" \
        "cutadapt>=4.9" \
 && mamba clean --all -f -y

# R + the core tidyverse/data-handling stack so the agent can compile
# Bioconductor packages at runtime via
# `Rscript -e 'BiocManager::install("DESeq2")'`. We deliberately do
# NOT pre-install specific Bioconductor analysis packages (DESeq2 /
# edgeR / clusterProfiler / Seurat / etc.) — that's an agent
# decision based on the SME-confirmed method. The agent has
# `r-base-dev` + dev headers in the apt layer above so source
# compiles work.
RUN mamba install -n base -y \
        "r-base>=4.3" \
        "r-tidyverse>=2.0" \
        "r-data.table>=1.15" \
        "r-ggplot2>=3.5" \
        "r-rmarkdown>=2.25" \
        "r-knitr>=1.45" \
        "r-biocmanager>=1.30" \
 && mamba clean --all -f -y

# Quarto for final-reporting atoms (S5.52). Pulled separately
# because it's not in conda-forge.
ARG QUARTO_VERSION=1.5.56
# F-DEP-L-05: pin sha256 against the upstream release artifact resolved
# 2026-05-14. Refresh together with QUARTO_VERSION on deliberate bumps.
ARG QUARTO_SHA256=75d19bc3e8fff5139a66ac3e4ab3f48a5dfee91e0ce7a90f488f262a935ee469
RUN curl -fsSL -o /tmp/quarto.deb \
        "https://github.com/quarto-dev/quarto-cli/releases/download/v${QUARTO_VERSION}/quarto-${QUARTO_VERSION}-linux-amd64.deb" \
 && echo "${QUARTO_SHA256}  /tmp/quarto.deb" | sha256sum -c - \
 && apt-get update \
 && apt-get install -y --no-install-recommends /tmp/quarto.deb \
 && rm /tmp/quarto.deb \
 && rm -rf /var/lib/apt/lists/*

# ─────────────────────────────────────────────────────────────────
# Stage 3: agent runtime. Claude Code needs Node.js + the agent
# script. The agent script itself is mounted at runtime by the
# harness, so we only install the runtime here.
# ─────────────────────────────────────────────────────────────────
ARG NODE_VERSION=20
# F-DEP-L-05: download-then-verify pattern for NodeSource installer.
# The upstream URL is mutable (NodeSource regenerates the script on
# every fetch with embedded GPG metadata), so we record a snapshot
# sha256 taken at 2026-05-14 and document the refresh procedure inline.
# TODO(phase-5-2026-05-14): rotate NODESOURCE_SETUP_SHA256 with each
# operator-driven base-image rebuild — pull the script, sha256sum it,
# inspect for unexpected diffs against the previous pin, then commit.
ARG NODESOURCE_SETUP_SHA256=2c4c6683a17b6f4128898a7b521e3c8bb725a99ffaf1b5e32ac97c6fa7d381be
RUN curl -fsSL "https://deb.nodesource.com/setup_${NODE_VERSION}.x" -o /tmp/ns.sh \
 && echo "${NODESOURCE_SETUP_SHA256}  /tmp/ns.sh" | sha256sum -c - \
 && bash /tmp/ns.sh \
 && rm /tmp/ns.sh \
 && apt-get update \
 && apt-get install -y --no-install-recommends nodejs \
 && rm -rf /var/lib/apt/lists/* \
 && npm install -g @anthropic-ai/claude-code

# Verify the toolchain came up. `mamba list` is the source of truth
# for "which package versions actually landed"; the dump is pruned
# to the toolchain-enabler lines that downstream pinning cares
# about. Specific Bioconductor / analysis packages are NOT enforced
# here because the agent installs them at runtime — listing them
# would falsely promise their presence.
RUN mkdir -p /etc/bio-min \
 && python -c 'import json,subprocess; \
    raw = subprocess.check_output(["mamba","list","--json"]).decode(); \
    items = json.loads(raw); \
    keep = ["python","numpy","pandas","scipy","libblas","liblapack","openblas","llvm-openmp","tbb","openmpi","samtools","bcftools","bwa","bowtie2","hisat2","star","salmon","kallisto","subread","gatk4","fastqc","multiqc","r-base","r-biocmanager","r-tidyverse"]; \
    out = {p["name"]: p["version"] for p in items if p["name"] in keep}; \
    out["claude-code"] = subprocess.check_output(["claude","--version"]).decode().strip(); \
    print(json.dumps(out, indent=2))' > /etc/bio-min/RUNTIME_VERSIONS.json \
 && python -c 'import ctypes.util; missing=[lib for lib in ("openblas","gomp","tbb","mpi","hdf5","zstd") if ctypes.util.find_library(lib) is None]; assert not missing, missing; print("native scientific libraries present")' \
 && Rscript --vanilla -e 'blas <- extSoftVersion()[["BLAS"]]; cat("R BLAS:", blas, "\n")' \
 && cat /etc/bio-min/RUNTIME_VERSIONS.json

# Working dir conventions — the harness mounts:
#   /work/inputs   (read-only intake + reference data)
#   /work/scratch  (per-task scratch)
#   /work/outputs  (results + state.patch.json)
WORKDIR /work
RUN mkdir -p /work/inputs /work/scratch /work/outputs

# Default to a non-root user to play nicely with bind mounts on
# operator hosts. The agent script may need root for some apt
# installs; operators who need root can run with `--user 0:0`.
RUN useradd --create-home --uid 1001 bio
USER bio:bio

# NO ENTRYPOINT. The harness's `scripts/agent-claude.sh` invokes
# the image as `docker run … $IMAGE claude --dangerously-skip-
# permissions … -p "$PROMPT"` — i.e. it passes the full executable
# path as the docker CMD. With `ENTRYPOINT ["/bin/bash"]` the
# `claude` arg gets interpreted as a script-file path instead of a
# command on PATH, which broke the first bio-min:local agent
# dispatch (2026-05-24). Leaving entrypoint unset matches the
# `ecaa-workflow-agent:local` baseline so agent-claude.sh works
# byte-identically against either image.
