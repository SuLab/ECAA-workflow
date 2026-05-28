# Bioinformatics GPU agent container.
#
# Build:
#   docker build -t scripps/bioinformatics-gpu:13.2.1 \
#     -f containers/bioinformatics-gpu/Dockerfile containers/bioinformatics-gpu/
#
# Purpose: CUDA/cuDNN-capable sibling of containers/bioinformatics for
# workloads whose framework cannot rely on CPU-only system libraries:
# scVI/PyTorch CUDA, RAPIDS-singlecell, AlphaFold-class inference, and
# custom CUDA extensions. It intentionally does not ship proprietary
# Parabricks, DeepVariant, AlphaFold DBs, or model weights; those stay in
# task-specific images/AMIs or mounted data volumes.

# F-DEP-L-04: pin by digest in addition to tag so a CDN/registry swap
# can't substitute a tampered nvidia/cuda image. Refresh digest on
# deliberate CUDA bumps (run `docker buildx imagetools inspect <tag>`).
ARG CUDA_IMAGE=nvidia/cuda:13.2.1-cudnn-devel-ubuntu24.04@sha256:6435dc5a825b0095648d87a3c91240fd7788a85fafaf215739544d389ab74366
FROM ${CUDA_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility \
    OMP_NUM_THREADS=1 \
    OPENBLAS_NUM_THREADS=1 \
    MKL_NUM_THREADS=1 \
    NUMEXPR_NUM_THREADS=1 \
    TBB_NUM_THREADS=1

# Ubuntu CUDA images may not have universe enabled; several scientific
# utilities live there. Enable it before installing the native compute
# surface.
RUN apt-get update \
    && apt-get install -y --no-install-recommends software-properties-common \
    && add-apt-repository -y universe \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl git git-lfs jq gnupg procps less file unzip zip rsync aria2 \
        build-essential pkg-config cmake ninja-build meson gfortran clang \
        rustc cargo \
        python3 python3-dev python3-venv python3-pip \
        r-base r-base-dev \
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
    && ldconfig -p | grep -q libcudnn \
    && ldconfig -p | grep -q libcudart \
    && Rscript --vanilla -e 'blas <- extSoftVersion()[["BLAS"]]; stopifnot(grepl("openblas|blas", tolower(blas))); cat("R BLAS:", blas, "\n")' \
    && python3 -c 'import ctypes.util; missing=[lib for lib in ("openblas","gomp","tbb","mpi","hdf5","zstd","cudart","cudnn") if ctypes.util.find_library(lib) is None]; assert not missing, missing; print("native CPU/GPU scientific libraries present")' \
    && nvcc --version >/dev/null \
    && rm -rf /var/lib/apt/lists/*

# Node 20 LTS via NodeSource. Claude Code CLI requires node>=18.
# F-DEP-L-05: download-then-verify (NodeSource regenerates the script
# per fetch; the SHA below is the 2026-05-14 snapshot).
# TODO(phase-5-2026-05-14): refresh NODESOURCE_SETUP_SHA256 on every
# base-image rebuild.
ARG NODESOURCE_SETUP_SHA256=2c4c6683a17b6f4128898a7b521e3c8bb725a99ffaf1b5e32ac97c6fa7d381be
RUN curl -fsSL https://deb.nodesource.com/setup_20.x -o /tmp/ns.sh \
    && echo "${NODESOURCE_SETUP_SHA256}  /tmp/ns.sh" | sha256sum -c - \
    && bash /tmp/ns.sh \
    && rm /tmp/ns.sh \
    && apt-get update \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code

RUN echo 'options(repos = c(CRAN = "https://cloud.r-project.org"))' \
    > /etc/R/Rprofile.site

WORKDIR /work
CMD ["bash", "-lc", "nvidia-smi && Rscript -e 'cat(R.version.string, \"\\n\")' && python3 --version && claude --version"]
